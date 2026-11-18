//! Intentionally small desktop/web host. All I/O and platform lifecycle stay here.
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "This isolated example is a host application"
)]
mod backend;
mod book;
mod paging;
#[cfg(target_arch = "wasm32")]
mod web;

use anyhow::{Result, anyhow, ensure};
use backend::{Backend, Job, Response};
use flow_publication::{
    document::{
        pixels::{PixelBuffer, PixelFormat},
        settings::ReadingMode,
        viewer::{FontResource, PanAxis, PointerButtonInput, ViewerEffect},
    },
    reading_session::{
        LoadFailure, NavigationState, ReadingSession, ReadingSessionError, RequestId, SessionEffect,
    },
};
use std::{num::NonZeroU32, rc::Rc, sync::Arc};
use web_time::Instant;
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition},
    event::{ElementState, MouseButton, MouseScrollDelta, StartCause, TouchPhase, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Window, WindowId},
};

#[derive(Clone, Copy)]
enum Command {
    Open,
    Back,
    Previous,
    Next,
    Smaller,
    Larger,
    ToggleMode,
}
enum Event {
    #[cfg(not(target_arch = "wasm32"))]
    Response(Response),
    #[cfg(target_arch = "wasm32")]
    Command(Command),
    #[cfg(target_arch = "wasm32")]
    OpenBytes { name: String, bytes: Vec<u8> },
    #[cfg(target_arch = "wasm32")]
    Run(Job),
}

struct Display {
    // Drop the surface before its context/window.
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    _context: softbuffer::Context<Rc<Window>>,
    window: Rc<Window>,
    pixels: PixelBuffer,
}
struct App {
    display: Option<Display>,
    backend: Backend,
    session: Option<ReadingSession>,
    generation: u64,
    requested_generation: u64,
    opening: bool,
    status: String,
    title: String,
    fonts: Vec<FontResource>,
    started: Instant,
    modifiers: ModifiersState,
    touch: Option<(u64, PhysicalPosition<f64>, PhysicalPosition<f64>)>,
    poll_again: bool,
    paging: paging::Paging,
    last_error: Option<String>,
}

pub fn run() -> Result<()> {
    let event_loop = EventLoop::<Event>::with_user_event().build()?;
    #[cfg(target_arch = "wasm32")]
    web::set_proxy(event_loop.create_proxy());
    let backend = Backend::new(event_loop.create_proxy())?;
    backend.submit(Job::OpenBytes {
        generation: 0,
        name: "Welcome.md".into(),
        bytes: book::WELCOME.as_bytes().to_vec(),
    })?;
    let app = App {
        display: None,
        backend,
        session: None,
        generation: 0,
        requested_generation: 0,
        opening: false,
        status: "Opening…".into(),
        title: "flow demo".into(),
        fonts: demo_fonts(),
        started: Instant::now(),
        modifiers: ModifiersState::empty(),
        touch: None,
        poll_again: false,
        paging: paging::Paging::default(),
        last_error: None,
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut app = app;
        event_loop.run_app(&mut app)?;
    }
    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        event_loop.spawn_app(app);
    }
    Ok(())
}

impl App {
    fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
        if let Some(display) = &self.display {
            display.window.set_title(&format!(
                "flow — {}",
                self.status.chars().take(180).collect::<String>()
            ));
        }
        #[cfg(target_arch = "wasm32")]
        web::status(&self.status);
    }
    fn error(&mut self, error: impl std::fmt::Display) {
        self.last_error = Some(error.to_string());
        self.set_status(format!("Error: {error}"));
    }
    fn reading_status(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        if self.opening || !session.viewer().has_document() {
            return;
        }
        let mut status = self.title.clone();
        if let Some((index, total)) = session.reading_order_position() {
            status.push_str(&format!(" — doc {index}/{total}"));
        }
        if session.settings().reading_mode == ReadingMode::Paginated {
            if let Some((page, total)) = session.viewer().paginated_page_metrics() {
                status.push_str(&format!(" · page {page}/{total}"));
            }
        } else {
            status.push_str(" · continuous");
        }
        #[cfg(target_arch = "wasm32")]
        web::mode(session.settings().reading_mode == ReadingMode::Paginated);
        if let Some(error) = &self.last_error {
            status.push_str(&format!(" — Error: {error}"));
        }
        self.set_status(status);
    }
    fn redraw(&self) {
        if let Some(display) = &self.display {
            display.window.request_redraw();
        }
    }
    fn begin_open(&mut self) -> Option<u64> {
        if self.opening {
            return None;
        }
        self.requested_generation = self.requested_generation.checked_add(1)?;
        self.opening = true;
        self.last_error = None;
        self.set_status("Opening…");
        Some(self.requested_generation)
    }
    fn submit_open(&mut self, job: Job) -> Result<()> {
        let result = self.backend.submit(job);
        if result.is_err() {
            self.opening = false;
        }
        result
    }
    fn command(&mut self, command: Command) -> Result<()> {
        if matches!(command, Command::Open) {
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(generation) = self.begin_open() {
                self.submit_open(Job::OpenPath {
                    generation,
                    path: None,
                })?;
            }
            #[cfg(target_arch = "wasm32")]
            web::pick_file()?;
            return Ok(());
        }
        let Some(session) = &mut self.session else {
            return Ok(());
        };
        match command {
            Command::Back => {
                self.paging.reset();
                session.go_back()?;
            }
            Command::Previous => self.paging.turn(session, -1)?,
            Command::Next => self.paging.turn(session, 1)?,
            Command::ToggleMode => {
                self.paging.cancel_tail();
                let mode = if session.settings().reading_mode == ReadingMode::Continuous {
                    ReadingMode::Paginated
                } else {
                    ReadingMode::Continuous
                };
                session.set_reading_mode(mode);
            }
            Command::Smaller | Command::Larger => {
                let delta = if matches!(command, Command::Smaller) {
                    -1.0
                } else {
                    1.0
                };
                session.set_font_size((session.settings().font_size + delta).clamp(8.0, 36.0));
            }
            Command::Open => unreachable!(),
        }
        self.pump()
    }
    fn pump(&mut self) -> Result<()> {
        let Some(session) = &mut self.session else {
            return Ok(());
        };
        let effects = session.take_effects()?;
        for effect in effects {
            let work = match effect {
                SessionEffect::LoadDocument(request) => Some((
                    request.id,
                    Job::Document {
                        generation: self.generation,
                        request,
                    },
                )),
                SessionEffect::LoadResource(request) => Some((
                    request.id,
                    Job::Resource {
                        generation: self.generation,
                        request,
                    },
                )),
                SessionEffect::ResolveLink {
                    id, destination, ..
                } => Some((
                    id,
                    Job::Link {
                        generation: self.generation,
                        id,
                        destination,
                    },
                )),
                SessionEffect::ResolvePublication { id, .. } => {
                    self.fail(id, "The demo has only one open publication".into());
                    None
                }
                SessionEffect::OpenExternal(reference) => {
                    self.open_external(&reference)?;
                    None
                }
                SessionEffect::NavigationFinished { id, state } => {
                    if let Some(session) = &self.session {
                        self.paging.finished(id, &state, session);
                    }
                    match state {
                        NavigationState::Failed(failure) => self.error(failure.message()),
                        NavigationState::Ready { .. } => {
                            self.last_error = None;
                            self.reading_status();
                        }
                        _ => (),
                    }
                    self.redraw();
                    None
                }
                SessionEffect::Viewer(ViewerEffect::Redraw) => {
                    self.redraw();
                    None
                }
                SessionEffect::Viewer(_) => None,
                SessionEffect::ContinueWork => {
                    self.poll_again = true;
                    None
                }
                SessionEffect::PublicationPaginationChanged
                | SessionEffect::CancelRequest(_)
                | SessionEffect::Reprioritize { .. } => None,
            };
            if let Some((id, work)) = work
                && let Err(error) = self.backend.submit(work)
            {
                self.fail(id, error.to_string());
            }
        }
        Ok(())
    }
    fn fail(&mut self, id: RequestId, error: String) {
        if let Some(session) = &mut self.session
            && matches!(
                session.fail_request(id, LoadFailure::new(&error)),
                Err(ReadingSessionError::StaleRequest)
            )
        {
            return;
        }
        self.error(error);
    }
    fn supplied(&mut self, id: RequestId, result: Result<(), ReadingSessionError>) {
        if let Err(error) = result
            && !matches!(error, ReadingSessionError::StaleRequest)
        {
            self.fail(id, error.to_string());
        }
    }
    fn response(&mut self, response: Response) -> Result<()> {
        match response {
            Response::Opened { generation, result } => {
                if generation != self.requested_generation
                    && !(generation == 0 && self.session.is_none())
                {
                    return Ok(());
                }
                if generation == self.requested_generation {
                    self.opening = false;
                }
                match result {
                    Ok(Some((title, model))) => {
                        let start = model
                            .start()
                            .ok_or_else(|| anyhow!("Publication has no start"))?;
                        let mut settings = self
                            .session
                            .as_ref()
                            .map(|session| session.settings().clone())
                            .unwrap_or_default();
                        settings.rendering.body_fonts = vec!["Charter".into()];
                        settings.rendering.code_fonts = vec!["Fira Code".into()];
                        let mut session = ReadingSession::new(settings);
                        // No speculative imports or whole-book pagination in this demo.
                        session.set_background_request_limit(0);
                        session.set_cache_limits(2, 2);
                        for font in &self.fonts {
                            ensure!(
                                session.register_font_resource(font.clone()),
                                "Invalid demo font"
                            );
                        }
                        if let Some(display) = &self.display {
                            let (width, height) = viewport(&display.window);
                            session.resize((width, height));
                        }
                        let handle = session.register_publication(model)?;
                        session.open_target(handle, start)?;
                        self.generation = generation;
                        self.paging.reset();
                        self.last_error = None;
                        self.session = Some(session);
                        self.title = title;
                        self.set_status("Loading document…");
                        self.redraw();
                    }
                    Ok(None) => self.set_status(self.title.clone()),
                    Err(error) => self.error(error),
                }
            }
            Response::Document {
                generation,
                id,
                result,
            } if generation == self.generation => match result {
                Ok(document) => {
                    if let Some(session) = &mut self.session {
                        let result = session.provide_document(id, document);
                        self.supplied(id, result);
                    }
                }
                Err(error) => self.fail(id, error),
            },
            Response::Resource {
                generation,
                id,
                result,
            } if generation == self.generation => match result {
                Ok(resource) => {
                    if let Some(session) = &mut self.session {
                        let result = session.provide_resource(id, resource);
                        self.supplied(id, result);
                    }
                }
                Err(error) => self.fail(id, error),
            },
            Response::Link {
                generation,
                id,
                result,
            } if generation == self.generation => match result {
                Ok(target) => {
                    if let Some(session) = &mut self.session {
                        let result = session.provide_link_target(id, target);
                        self.supplied(id, result);
                    }
                }
                Err(error) => self.fail(id, error),
            },
            _ => (), // Results from a replaced open document are never installed.
        }
        self.pump()
    }
    fn open_external(&self, reference: &str) -> Result<()> {
        let url = book::external_url(reference)?;
        #[cfg(not(target_arch = "wasm32"))]
        webbrowser::open(url.as_str())?;
        #[cfg(target_arch = "wasm32")]
        web::open_external(url.as_str())?;
        Ok(())
    }
    fn paint(&mut self) -> Result<()> {
        let Some(display) = &mut self.display else {
            return Ok(());
        };
        let (logical_width, logical_height) = viewport(&display.window);
        let scale = render_scale(&display.window);
        let (raster_width, raster_height) =
            flow_publication::document::render::render_surface_size(
                logical_width as f32,
                logical_height as f32,
                scale,
            )?;
        #[cfg(not(target_arch = "wasm32"))]
        let size = display.window.inner_size();
        #[cfg(target_arch = "wasm32")]
        let size = winit::dpi::PhysicalSize::new(raster_width, raster_height);
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return Ok(());
        };
        ensure!(
            u64::from(raster_width) * u64::from(raster_height) <= 16 * 1024 * 1024,
            "Window exceeds demo's 16 megapixel limit"
        );
        display
            .surface
            .resize(width, height)
            .map_err(|error| anyhow!("{error}"))?;
        display
            .pixels
            .resize_and_clear(raster_width, raster_height, [255; 3])?;
        if let Some(session) = &mut self.session
            && session.viewer().has_document()
        {
            session.resize((logical_width, logical_height));
            self.paging.before_paint(session);
            session.render_into(
                &mut display.pixels.surface_mut(),
                logical_width,
                logical_height,
                scale,
            )?;
        }
        let mut buffer = display
            .surface
            .buffer_mut()
            .map_err(|error| anyhow!("{error}"))?;
        pack_pixels(&display.pixels, &mut buffer, size.width as usize);
        buffer.present().map_err(|error| anyhow!("{error}"))?;
        self.reading_status();
        self.pump()
    }
    fn keyboard(&mut self, key: &Key) -> Result<()> {
        let command = self.modifiers.control_key() || self.modifiers.super_key();
        let paginated = self
            .session
            .as_ref()
            .is_some_and(|session| session.settings().reading_mode == ReadingMode::Paginated);
        if let Some(session) = &mut self.session {
            match key {
                Key::Named(NamedKey::Tab) => {
                    session.focus_adjacent(if self.modifiers.shift_key() { -1 } else { 1 }, false);
                    return self.pump();
                }
                Key::Named(NamedKey::Escape) => {
                    session.clear_focus();
                    return self.pump();
                }
                Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight)
                    if !self.modifiers.alt_key() =>
                {
                    let delta = if *key == Key::Named(NamedKey::ArrowLeft) {
                        -40.0
                    } else {
                        40.0
                    };
                    if session.scroll_focused_horizontal(delta) {
                        return self.pump();
                    }
                }
                _ => (),
            }
        }
        let action = match key {
            Key::Character(value) if !command && value.eq_ignore_ascii_case("p") => {
                Some(Command::ToggleMode)
            }
            Key::Named(NamedKey::PageDown | NamedKey::ArrowDown) if paginated => {
                Some(Command::Next)
            }
            Key::Named(NamedKey::PageUp | NamedKey::ArrowUp) if paginated => {
                Some(Command::Previous)
            }
            Key::Named(NamedKey::Space) if paginated => Some(if self.modifiers.shift_key() {
                Command::Previous
            } else {
                Command::Next
            }),
            Key::Character(value) if command && value.eq_ignore_ascii_case("o") => {
                Some(Command::Open)
            }
            Key::Character(value) if command && matches!(value.as_str(), "+" | "=") => {
                Some(Command::Larger)
            }
            Key::Character(value) if command && value.as_str() == "-" => Some(Command::Smaller),
            Key::Named(NamedKey::ArrowLeft) if self.modifiers.alt_key() => Some(Command::Back),
            Key::Named(NamedKey::Backspace) => Some(Command::Back),
            Key::Named(NamedKey::ArrowLeft) => Some(Command::Previous),
            Key::Named(NamedKey::ArrowRight) => Some(Command::Next),
            _ => None,
        };
        if let Some(action) = action {
            return self.command(action);
        }
        if let Some(session) = &mut self.session {
            let height = session.viewer().viewport_size().1 as f32;
            let delta = match key {
                Key::Named(NamedKey::PageDown) => Some(height * 0.8),
                Key::Named(NamedKey::PageUp) => Some(-height * 0.8),
                Key::Named(NamedKey::ArrowDown) => Some(40.0),
                Key::Named(NamedKey::ArrowUp) => Some(-40.0),
                Key::Named(NamedKey::Space) => Some(
                    height
                        * if self.modifiers.shift_key() {
                            -0.8
                        } else {
                            0.8
                        },
                ),
                _ => None,
            };
            if let Some(delta) = delta {
                session.scroll_by(delta);
            }
        }
        self.pump()
    }
}

impl ApplicationHandler<Event> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.display.is_some() {
            return;
        }
        let result = (|| -> Result<Display> {
            let attributes = Window::default_attributes()
                .with_title("flow demo")
                .with_inner_size(LogicalSize::new(800, 700));
            #[cfg(target_arch = "wasm32")]
            let attributes = web::window_attributes(attributes)?;
            let window = Rc::new(event_loop.create_window(attributes)?);
            let context =
                softbuffer::Context::new(window.clone()).map_err(|error| anyhow!("{error}"))?;
            let surface = softbuffer::Surface::new(&context, window.clone())
                .map_err(|error| anyhow!("{error}"))?;
            Ok(Display {
                surface,
                _context: context,
                window,
                pixels: PixelBuffer::default(),
            })
        })();
        match result {
            Ok(display) => {
                self.display = Some(display);
                self.set_status(self.status.clone());
                self.redraw();
            }
            Err(error) => {
                self.error(error);
                event_loop.exit();
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.requested_generation == 0
            && let Some(path) = std::env::args_os().nth(1)
            && let Some(generation) = self.begin_open()
            && let Err(error) = self.submit_open(Job::OpenPath {
                generation,
                path: Some(path.into()),
            })
        {
            self.opening = false;
            self.error(error);
        }
    }
    fn suspended(&mut self, _: &ActiveEventLoop) {
        self.display = None;
    }
    fn new_events(&mut self, _: &ActiveEventLoop, _: StartCause) {
        if let Some(session) = &mut self.session {
            session.tick(self.started.elapsed());
        }
        if let Err(error) = self.pump() {
            self.error(error);
        }
    }
    fn user_event(&mut self, _: &ActiveEventLoop, event: Event) {
        let result = match event {
            #[cfg(not(target_arch = "wasm32"))]
            Event::Response(response) => self.response(response),
            #[cfg(target_arch = "wasm32")]
            Event::Run(job) => {
                let response = self.backend.run(job);
                self.response(response)
            }
            #[cfg(target_arch = "wasm32")]
            Event::Command(command) => self.command(command),
            #[cfg(target_arch = "wasm32")]
            Event::OpenBytes { name, bytes } => {
                if let Some(generation) = self.begin_open() {
                    self.submit_open(Job::OpenBytes {
                        generation,
                        name,
                        bytes,
                    })
                } else {
                    Ok(())
                }
            }
        };
        if let Err(error) = result {
            self.opening = false;
            self.error(error);
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, window: WindowId, event: WindowEvent) {
        if self
            .display
            .as_ref()
            .is_none_or(|display| display.window.id() != window)
        {
            return;
        }
        let result = (|| -> Result<()> {
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::RedrawRequested => self.paint()?,
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => self.redraw(),
                WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed =>
                {
                    self.keyboard(&event.logical_key)?
                }
                #[cfg(not(target_arch = "wasm32"))]
                WindowEvent::DroppedFile(path) => {
                    if let Some(generation) = self.begin_open() {
                        self.submit_open(Job::OpenPath {
                            generation,
                            path: Some(path),
                        })?;
                    }
                }
                WindowEvent::Focused(false) => {
                    if let Some(session) = &mut self.session {
                        session.cancel_pending_interaction();
                    }
                }
                event => {
                    let scale = self
                        .display
                        .as_ref()
                        .expect("checked display")
                        .window
                        .scale_factor();
                    let now = self.started.elapsed();
                    if let Some(session) = &mut self.session {
                        match event {
                            WindowEvent::MouseWheel { delta, .. } => {
                                let (x, y) = match delta {
                                    MouseScrollDelta::LineDelta(x, y) => (-x * 44.0, -y * 44.0),
                                    MouseScrollDelta::PixelDelta(position) => {
                                        ((-position.x / scale) as f32, (-position.y / scale) as f32)
                                    }
                                };
                                if x.abs() > y.abs() && session.wheel_horizontal(x) {
                                    self.paging.cancel_tail();
                                } else {
                                    self.paging.wheel(session, y)?;
                                }
                            }
                            WindowEvent::CursorMoved { position, .. } => session.pointer_moved(
                                Some(((position.x / scale) as f32, (position.y / scale) as f32)),
                                now,
                            ),
                            WindowEvent::CursorLeft { .. } => session.pointer_moved(None, now),
                            WindowEvent::MouseInput {
                                state,
                                button: MouseButton::Left,
                                ..
                            } => {
                                if state == ElementState::Pressed {
                                    self.paging.cancel_tail();
                                }
                                session.pointer_button(
                                    PointerButtonInput {
                                        pressed: state == ElementState::Pressed,
                                        click_count: 1,
                                    },
                                    now,
                                );
                            }
                            WindowEvent::Touch(touch) => match touch.phase {
                                TouchPhase::Started => {
                                    session.begin_pan(
                                        (touch.location.x / scale) as f32,
                                        (touch.location.y / scale) as f32,
                                    );
                                    self.touch = Some((touch.id, touch.location, touch.location))
                                }
                                TouchPhase::Moved => {
                                    if let Some((id, _, last)) = &mut self.touch
                                        && *id == touch.id
                                    {
                                        let captured = session.pan_to(
                                            (touch.location.x / scale) as f32,
                                            (touch.location.y / scale) as f32,
                                        );
                                        if !captured
                                            && session.viewer().pan_axis()
                                                == Some(PanAxis::Vertical)
                                            && session.settings().reading_mode
                                                == ReadingMode::Continuous
                                        {
                                            session.scroll_by(
                                                ((last.y - touch.location.y) / scale) as f32,
                                            );
                                        }
                                        *last = touch.location;
                                    }
                                }
                                TouchPhase::Ended => {
                                    if let Some((id, start, _)) = self.touch.take()
                                        && id == touch.id
                                    {
                                        let captured = session.pan_to(
                                            (touch.location.x / scale) as f32,
                                            (touch.location.y / scale) as f32,
                                        );
                                        let axis = session.viewer().pan_axis();
                                        session.end_pan();
                                        if captured {
                                            self.paging.cancel_tail();
                                        }
                                        let dx = (start.x - touch.location.x) / scale;
                                        let dy = (start.y - touch.location.y) / scale;
                                        let delta = if axis == Some(PanAxis::Horizontal) {
                                            dx
                                        } else {
                                            dy
                                        };
                                        if !captured
                                            && session.settings().reading_mode
                                                == ReadingMode::Paginated
                                            && delta.abs() >= 40.0
                                        {
                                            self.paging
                                                .turn(session, if delta > 0.0 { 1 } else { -1 })?;
                                        } else if dx.hypot(dy) < 5.0 {
                                            self.paging.cancel_tail();
                                            session.pointer_moved(
                                                Some((
                                                    (touch.location.x / scale) as f32,
                                                    (touch.location.y / scale) as f32,
                                                )),
                                                now,
                                            );
                                            session.pointer_button(
                                                PointerButtonInput {
                                                    pressed: true,
                                                    click_count: 1,
                                                },
                                                now,
                                            );
                                            session.pointer_button(
                                                PointerButtonInput {
                                                    pressed: false,
                                                    click_count: 1,
                                                },
                                                now,
                                            );
                                        }
                                    }
                                }
                                TouchPhase::Cancelled => {
                                    session.end_pan();
                                    self.touch = None;
                                }
                            },
                            _ => (),
                        }
                    }
                }
            }
            self.pump()
        })();
        if let Err(error) = result {
            self.error(error);
        }
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let control = if std::mem::take(&mut self.poll_again) {
            ControlFlow::Poll
        } else if let Some(deadline) = self
            .session
            .as_ref()
            .and_then(|session| session.viewer().next_wakeup())
        {
            ControlFlow::WaitUntil(self.started + deadline)
        } else {
            ControlFlow::Wait
        };
        event_loop.set_control_flow(control);
    }
}

fn viewport(window: &Window) -> (u32, u32) {
    #[cfg(target_arch = "wasm32")]
    if let Some(size) = web::logical_size() {
        return size;
    }
    logical_extent(window.inner_size(), window.scale_factor())
}
fn logical_extent(size: winit::dpi::PhysicalSize<u32>, scale: f64) -> (u32, u32) {
    (
        (size.width as f64 / scale).ceil().max(1.0) as u32,
        (size.height as f64 / scale).ceil().max(1.0) as u32,
    )
}
fn render_scale(window: &Window) -> f32 {
    #[cfg(target_arch = "wasm32")]
    if let Some(scale) = web::pixel_ratio() {
        return scale as f32;
    }
    window.scale_factor() as f32
}
fn pack_pixels(source: &PixelBuffer, destination: &mut [u32], width: usize) {
    // Integer logical viewports can require a raster one pixel larger than a
    // native window at fractional/retina scales. Crop rows, never flatten strides.
    for (target_row, source_row) in destination
        .chunks_exact_mut(width)
        .zip(source.pixels().chunks_exact(source.stride()))
    {
        for (target, pixel) in target_row.iter_mut().zip(source_row.chunks_exact(4)) {
            let (r, g, b) = match source.format() {
                PixelFormat::Bgra8888 => (pixel[2], pixel[1], pixel[0]),
                PixelFormat::Rgba8888 => (pixel[0], pixel[1], pixel[2]),
            };
            *target = u32::from_be_bytes([0, r, g, b]);
        }
    }
}
fn demo_fonts() -> Vec<FontResource> {
    // Reuse licensed repository fixtures rather than discovering system fonts.
    [
        include_bytes!("../../../crates/flow_document/tests/fixtures/fonts/charter.ttf").as_slice(),
        include_bytes!("../../../crates/flow_document/tests/fixtures/fonts/charter-bold.ttf")
            .as_slice(),
        include_bytes!("../../../crates/flow_document/tests/fixtures/fonts/charter-italic.ttf")
            .as_slice(),
        include_bytes!(
            "../../../crates/flow_document/tests/fixtures/fonts/charter-bold-italic.ttf"
        )
        .as_slice(),
        include_bytes!("../../../crates/flow_document/tests/fixtures/fonts/fira-code.ttf")
            .as_slice(),
    ]
    .into_iter()
    .map(|bytes| FontResource::from_bytes(Arc::from(bytes)))
    .collect()
}
