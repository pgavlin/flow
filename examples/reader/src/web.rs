//! Browser-only shell. Uploaded bytes are the only acquisition capability.
use crate::{Command, Event};
use anyhow::{Result, anyhow, ensure};
use std::cell::RefCell;
use wasm_bindgen::{JsCast, prelude::*};
use winit::{
    event_loop::EventLoopProxy, platform::web::WindowAttributesExtWebSys, window::WindowAttributes,
};

thread_local! { static PROXY: RefCell<Option<EventLoopProxy<Event>>> = const { RefCell::new(None) }; }
pub fn set_proxy(proxy: EventLoopProxy<Event>) {
    PROXY.with(|slot| *slot.borrow_mut() = Some(proxy));
}
fn send(event: Event) {
    PROXY.with(|slot| {
        if let Some(proxy) = slot.borrow().as_ref() {
            let _ = proxy.send_event(event);
        }
    });
}
fn document() -> Result<web_sys::Document> {
    web_sys::window()
        .and_then(|window| window.document())
        .ok_or_else(|| anyhow!("Browser document unavailable"))
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    crate::run().map_err(|error| JsValue::from_str(&format!("{error:#}")))
}
#[wasm_bindgen]
pub fn command(name: &str) {
    let action = match name {
        "open" => Command::Open,
        "back" => Command::Back,
        "previous" => Command::Previous,
        "next" => Command::Next,
        "smaller" => Command::Smaller,
        "larger" => Command::Larger,
        "mode" => Command::ToggleMode,
        _ => return,
    };
    send(Event::Command(action));
}
#[wasm_bindgen]
pub fn open_file(name: String, bytes: Vec<u8>) -> Result<(), JsValue> {
    ensure_size(bytes.len()).map_err(|error| JsValue::from_str(&error.to_string()))?;
    send(Event::OpenBytes { name, bytes });
    Ok(())
}
fn ensure_size(size: usize) -> Result<()> {
    ensure!(
        size <= crate::book::MAX_INPUT,
        "File exceeds the 64 MiB demo limit"
    );
    Ok(())
}

pub fn mode(paginated: bool) {
    if let Ok(document) = document() {
        if let Some(button) = document.get_element_by_id("mode") {
            let _ = button.set_attribute("aria-pressed", if paginated { "true" } else { "false" });
        }
        for (command, label) in [
            (
                "previous",
                if paginated {
                    "Previous page"
                } else {
                    "Previous document"
                },
            ),
            (
                "next",
                if paginated {
                    "Next page"
                } else {
                    "Next document"
                },
            ),
        ] {
            if let Ok(Some(button)) = document.query_selector(&format!("[data-command={command}]"))
            {
                let _ = button.set_attribute("aria-label", label);
            }
        }
    }
}

pub fn logical_size() -> Option<(u32, u32)> {
    let canvas = document().ok()?.get_element_by_id("reader")?;
    Some((
        canvas.client_width().max(1) as u32,
        canvas.client_height().max(1) as u32,
    ))
}
pub fn pixel_ratio() -> Option<f64> {
    Some(web_sys::window()?.device_pixel_ratio())
}

pub fn window_attributes(attributes: WindowAttributes) -> Result<WindowAttributes> {
    let canvas = document()?
        .get_element_by_id("reader")
        .ok_or_else(|| anyhow!("Missing reader canvas"))?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| anyhow!("Invalid reader canvas"))?;
    // CSS/ResizeObserver supplies the canvas size; do not set a desktop size.
    let mut attributes = attributes
        .with_canvas(Some(canvas))
        .with_prevent_default(true);
    attributes.inner_size = None;
    Ok(attributes)
}
pub fn pick_file() -> Result<()> {
    let input = document()?
        .get_element_by_id("file")
        .ok_or_else(|| anyhow!("Missing file input"))?
        .dyn_into::<web_sys::HtmlInputElement>()
        .map_err(|_| anyhow!("Invalid file input"))?;
    input.click();
    Ok(())
}
pub fn status(text: &str) {
    if let Ok(document) = document()
        && let Some(status) = document.get_element_by_id("status")
    {
        status.set_text_content(Some(text));
    }
}
pub fn open_external(url: &str) -> Result<()> {
    let opened = web_sys::window()
        .ok_or_else(|| anyhow!("No browser window"))?
        .open_with_url_and_target_and_features(url, "_blank", "noopener,noreferrer")
        .map_err(|_| anyhow!("Browser refused the external link"))?;
    // With noopener, browsers may return null even when successfully opened.
    let _ = opened;
    Ok(())
}
