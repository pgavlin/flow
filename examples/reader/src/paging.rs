//! Small host page-turn policy. Page geometry/position belong to the engine.
use flow_publication::{
    document::settings::ReadingMode,
    publication::{DocumentKey, registry::PublicationHandle},
    reading_session::{NavigationId, NavigationState, ReadingSession, ReadingSessionError},
};

#[derive(Default)]
pub(super) struct Paging {
    pending: Option<(NavigationId, bool)>,
    tail: Option<(PublicationHandle, DocumentKey)>,
    wheel: f32,
}
impl Paging {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn cancel_tail(&mut self) {
        self.tail = None;
        self.wheel = 0.0;
    }

    pub fn turn(
        &mut self,
        session: &mut ReadingSession,
        direction: isize,
    ) -> Result<(), ReadingSessionError> {
        if self
            .pending
            .is_some_and(|(id, _)| session.navigation_state(id) == Some(&NavigationState::Pending))
        {
            return Ok(());
        }
        // A second input can arrive before the redraw after a cached chapter load.
        self.before_paint(session);
        self.cancel_tail();
        session.cancel_pending_interaction();
        let paginated = session.settings().reading_mode == ReadingMode::Paginated;
        if paginated {
            let (width, height) = session.viewer().viewport_size();
            let Some((page, total)) = session.prepare_pagination(width, height) else {
                return Ok(());
            };
            if (direction < 0 && page > 1) || (direction > 0 && page < total) {
                session.scroll_by(direction.signum() as f32);
                return Ok(());
            }
        }
        self.pending = session
            .move_reading_order(direction)?
            .map(|id| (id, paginated && direction < 0));
        Ok(())
    }

    pub fn wheel(
        &mut self,
        session: &mut ReadingSession,
        delta: f32,
    ) -> Result<(), ReadingSessionError> {
        if session.settings().reading_mode == ReadingMode::Continuous {
            session.wheel(delta);
            return Ok(());
        }
        // One page per notch/gesture threshold, not one per tiny trackpad event.
        self.wheel += delta;
        if self.wheel.abs() >= 80.0 {
            let direction = if self.wheel < 0.0 { -1 } else { 1 };
            self.wheel = 0.0;
            self.turn(session, direction)?;
        }
        Ok(())
    }

    pub fn finished(
        &mut self,
        id: NavigationId,
        state: &NavigationState,
        session: &ReadingSession,
    ) {
        if let Some((pending, last)) = self.pending
            && id == pending
        {
            self.pending = None;
            if last && matches!(state, NavigationState::Ready { .. }) {
                self.tail = session.current_publication().zip(
                    session
                        .current_target()
                        .map(|target| target.document.clone()),
                );
            }
        }
    }

    pub fn before_paint(&mut self, session: &mut ReadingSession) {
        let Some((publication, document)) = &self.tail else {
            return;
        };
        if session.current_publication() != Some(*publication)
            || session
                .current_target()
                .is_none_or(|target| &target.document != document)
        {
            self.tail = None;
            return;
        }
        // Retain end-of-document intent across late intrinsic-image metadata.
        // The next explicit turn/click clears it. No whole-book index required.
        let (width, height) = session.viewer().viewport_size();
        match session.settings().reading_mode {
            ReadingMode::Paginated => {
                session.prepare_pagination(width, height);
            }
            ReadingMode::Continuous => {
                session.prepare_continuous_scene(width, height);
            }
        }
        session.scroll_to(f32::MAX);
    }
}
