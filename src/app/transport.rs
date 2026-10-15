//! One player-style transport bar, independent of workspace navigation.
use super::*;
use crate::music::SyncMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Start(SyncMode),
    Stop,
}

struct Control {
    label: &'static str,
    action: Option<Action>,
}

fn start_label(target: SyncMode, active: Option<SyncMode>, state: &SessionState) -> &'static str {
    match (target, active, state) {
        (SyncMode::Video, Some(SyncMode::Video), SessionState::Paused) => "Resume Video",
        (SyncMode::Music, Some(SyncMode::Music), SessionState::Paused) => "Resume Music",
        (SyncMode::Video, Some(SyncMode::Music), SessionState::Running | SessionState::Paused) => {
            "Switch to Video"
        }
        (SyncMode::Music, Some(SyncMode::Video), SessionState::Running | SessionState::Paused) => {
            "Switch to Music"
        }
        (SyncMode::Video, _, _) => "Start Video",
        (SyncMode::Music, _, _) => "Start Music",
    }
}

fn stop_label(mode: Option<SyncMode>) -> &'static str {
    match mode {
        Some(SyncMode::Video) => "Stop Video",
        Some(SyncMode::Music) => "Stop Music",
        None => "Stop",
    }
}

fn primary_control(
    workspace: Workspace,
    active: Option<SyncMode>,
    state: &SessionState,
) -> Control {
    let target = workspace.sync_mode();
    // While capture/audio startup is pending, cancel that request before trying
    // another mode. Stop must remain available from every workspace.
    let stop = *state == SessionState::RequestingPermission
        || (*state == SessionState::Running && (target.is_none() || target == active))
        || (target.is_none() && matches!(state, SessionState::Paused | SessionState::Error));
    if stop {
        Control {
            label: stop_label(active),
            action: Some(Action::Stop),
        }
    } else if let Some(target) = target {
        Control {
            label: start_label(target, active, state),
            action: Some(Action::Start(target)),
        }
    } else {
        Control {
            label: "Start",
            action: None,
        }
    }
}

fn button(
    ui: &mut egui::Ui,
    id: &str,
    rect: Rect,
    enabled: bool,
    button: egui::Button<'_>,
) -> egui::Response {
    ui.new_child(egui::UiBuilder::new().id_salt(id).max_rect(rect).layout(
        egui::Layout::centered_and_justified(egui::Direction::TopDown),
    ))
    .add_enabled(enabled, button)
}

impl App {
    /// Returns true when Start/Switch/Resume was requested, allowing the rest of
    /// this frame to lock hardware editing before the engine publishes startup.
    pub(super) fn transport_bar(
        &mut self,
        ctx: &egui::Context,
        snapshot: &crate::engine::Snapshot,
        validation: Option<&str>,
    ) -> bool {
        let active = snapshot
            .mode
            .or_else(|| (snapshot.state == SessionState::Paused).then_some(self.config.mode));
        let control = primary_control(self.workspace, active, &snapshot.state);
        let blocked = match control.action {
            None => Some("Open Video or Music to start synchronization."),
            Some(Action::Stop) => None,
            Some(Action::Start(_)) if self.busy => {
                Some("Wait for the device connection to finish.")
            }
            Some(Action::Start(_)) if self.ha_picker.is_some() || self.layout_dialog.is_some() => {
                Some("Finish or close the open dialog first.")
            }
            Some(Action::Start(_)) if validation.is_some() => {
                Some("Resolve the configuration message before starting.")
            }
            Some(Action::Start(_)) if self.config.lights.is_empty() => Some("Add a device first."),
            Some(Action::Start(_)) => None,
        };
        let mut started = false;
        egui::TopBottomPanel::bottom("transport").show(ctx, |ui| {
            if let Some(error) = validation {
                ui.label(
                    egui::RichText::new(format!("Configuration needs attention: {error}"))
                        .color(AMBER),
                );
            }
            if !self.notice.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    if ui.small_button("Dismiss").clicked() {
                        self.notice.clear();
                    }
                    ui.label(&self.notice);
                });
            }
            if snapshot.state == SessionState::Error {
                ui.label(egui::RichText::new(&snapshot.message).color(AMBER));
            }
            ui.add_space(8.0);
            let (row, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::hover());
            let primary = Rect::from_center_size(row.center(), Vec2::new(180.0, 38.0));
            let response = button(
                ui,
                "primary-transport",
                primary,
                blocked.is_none(),
                egui::Button::new(control.label).fill(if blocked.is_none() {
                    Color32::from_rgb(35, 89, 118)
                } else {
                    Color32::from_rgb(45, 51, 60)
                }),
            );
            if response.clicked() {
                match control.action {
                    Some(Action::Start(mode)) => {
                        self.config.mode = mode;
                        self.send(Command::Start(self.config.clone()));
                        started = true;
                    }
                    Some(Action::Stop) => self.send(Command::Stop),
                    None => {}
                }
            }
            if let Some(reason) = blocked {
                response.on_disabled_hover_text(reason);
            }
            ui.add_space(6.0);
        });
        started
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_actions_cover_start_stop_resume_switch_and_pending_capture() {
        use SessionState::{Error, Idle, Paused, RequestingPermission, Running};
        use SyncMode::{Music, Video};
        for (workspace, active, state, label, action) in [
            (
                Workspace::Video,
                None,
                Idle,
                "Start Video",
                Some(Action::Start(Video)),
            ),
            (
                Workspace::Music,
                None,
                Idle,
                "Start Music",
                Some(Action::Start(Music)),
            ),
            (
                Workspace::Video,
                Some(Video),
                Running,
                "Stop Video",
                Some(Action::Stop),
            ),
            (
                Workspace::Music,
                Some(Music),
                Running,
                "Stop Music",
                Some(Action::Stop),
            ),
            (
                Workspace::Video,
                Some(Music),
                Running,
                "Switch to Video",
                Some(Action::Start(Video)),
            ),
            (
                Workspace::Music,
                Some(Video),
                Running,
                "Switch to Music",
                Some(Action::Start(Music)),
            ),
            (
                Workspace::Video,
                Some(Video),
                Paused,
                "Resume Video",
                Some(Action::Start(Video)),
            ),
            (
                Workspace::Music,
                Some(Music),
                Paused,
                "Resume Music",
                Some(Action::Start(Music)),
            ),
            (
                Workspace::Video,
                Some(Music),
                Paused,
                "Switch to Video",
                Some(Action::Start(Video)),
            ),
            (
                Workspace::Music,
                Some(Video),
                Paused,
                "Switch to Music",
                Some(Action::Start(Music)),
            ),
            (
                Workspace::Music,
                Some(Video),
                RequestingPermission,
                "Stop Video",
                Some(Action::Stop),
            ),
            (
                Workspace::Video,
                None,
                Error,
                "Start Video",
                Some(Action::Start(Video)),
            ),
        ] {
            let control = primary_control(workspace, active, &state);
            assert_eq!(
                (control.label, control.action),
                (label, action),
                "{workspace:?} {state:?}"
            );
        }
        for workspace in [Workspace::Devices, Workspace::Layout, Workspace::Studio] {
            let control = primary_control(workspace, None, &Idle);
            assert_eq!((control.label, control.action), ("Start", None));
            for state in [Running, Paused, RequestingPermission] {
                for (mode, label) in [(Video, "Stop Video"), (Music, "Stop Music")] {
                    let control = primary_control(workspace, Some(mode), &state);
                    assert_eq!((control.label, control.action), (label, Some(Action::Stop)));
                }
            }
        }
    }
}
