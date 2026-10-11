//! UI state and its reducer. Pure (no JS calls) so it is unit-tested natively.
use std::rc::Rc;

use lightning_protocol::dto::{
    ApprovalRequestDto, LoadedSessionDto, LogIssuesDto, SessionInfoDto, SettingsDto, TurnFinishedDto,
};
use lightning_protocol::view::TimelineModel;
use lightning_protocol::{Event, SessionStatus, Summary};
use yew::Reducible;

#[derive(Debug, Clone, PartialEq)]
pub struct LiveSession {
    /// Filled by `start_session`; events can arrive before that call returns.
    pub info: Option<SessionInfoDto>,
    pub model: TimelineModel,
    /// How the last turn ended. Anything but `Completed` means: start a new session.
    pub last_status: Option<SessionStatus>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryView {
    pub model: TimelineModel,
    pub issues: LogIssuesDto,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BannerKind {
    Error,
    Info,
    Warn,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Banner {
    pub kind: BannerKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppState {
    pub settings: Option<SettingsDto>,
    pub workspace: Option<String>,
    pub sessions: Vec<Summary>,
    pub live: Option<LiveSession>,
    /// When set, the main pane shows this recorded session instead of the live one.
    pub history: Option<HistoryView>,
    pub approval: Option<ApprovalRequestDto>,
    pub banner: Option<Banner>,
    /// A turn is in flight.
    pub running: bool,
    pub cancelling: bool,
    pub settings_open: bool,
}

pub enum Action {
    SettingsLoaded(SettingsDto),
    WorkspaceChosen(String),
    SessionsLoaded(Vec<Summary>),
    SessionStarted(SessionInfoDto),
    /// Live event from the backend.
    Event(Event),
    Approval(ApprovalRequestDto),
    ApprovalClosed(String),
    Sending,
    Cancelling,
    /// Starting the session or sending the message failed before the turn began.
    SendFailed(String),
    TurnFinished(TurnFinishedDto),
    HistoryLoaded(LoadedSessionDto),
    CloseHistory,
    NewSession,
    /// A live session found already running after a reload.
    Restored { info: SessionInfoDto, events: Vec<Event>, running: bool },
    Error(String),
    Notice(String),
    DismissBanner,
    SettingsOpen(bool),
}

impl AppState {
    pub fn key_present(&self) -> bool {
        self.settings.as_ref().is_some_and(|s| s.key_present)
    }

    /// The live session accepts another message only after a completed turn.
    pub fn live_can_continue(&self) -> bool {
        self.live.as_ref().is_none_or(|l| matches!(l.last_status, None | Some(SessionStatus::Completed)))
    }

    /// Shown when nothing else is: the app cannot do anything useful without a key.
    pub fn missing_key_banner(&self) -> Option<Banner> {
        (self.settings.is_some() && !self.key_present() && self.banner.is_none())
            .then(|| Banner { kind: BannerKind::Warn, text: "Add your Gemini API key to get started.".into() })
    }

    pub fn can_send(&self) -> bool {
        self.key_present() && self.workspace.is_some() && !self.running && self.history.is_none() && self.live_can_continue()
    }

    fn banner_for(done: &TurnFinishedDto) -> Option<Banner> {
        let text = match done.status {
            SessionStatus::Completed | SessionStatus::Cancelled => return None,
            SessionStatus::LlmError => format!("The model could not be reached: {}", done.error.as_deref().unwrap_or("unknown error")),
            SessionStatus::InternalError => done.error.clone().unwrap_or_else(|| "The assistant stopped unexpectedly.".into()),
            SessionStatus::MaxStepsReached => "The assistant stopped after too many steps. Start a new session to continue.".into(),
            SessionStatus::Stuck => "The assistant kept repeating a failing call and was stopped.".into(),
            SessionStatus::ContextLimit => "The conversation got too long. Start a new session.".into(),
            SessionStatus::TruncatedOutput => "The reply was cut off. Start a new session or ask again.".into(),
        };
        Some(Banner { kind: BannerKind::Error, text })
    }
}

impl Reducible for AppState {
    type Action = Action;

    fn reduce(self: Rc<Self>, action: Action) -> Rc<Self> {
        let mut s = (*self).clone();
        match action {
            Action::SettingsLoaded(settings) => {
                if s.workspace.is_none() {
                    s.workspace = settings.last_workspace.clone();
                }
                s.settings = Some(settings);
            }
            Action::WorkspaceChosen(ws) => {
                if s.workspace.as_deref() != Some(ws.as_str()) {
                    s.sessions.clear();
                    s.live = None;
                    s.running = false;
                    s.cancelling = false;
                    s.approval = None;
                }
                s.workspace = Some(ws);
                s.history = None;
            }
            Action::SessionsLoaded(list) => s.sessions = list,
            Action::SessionStarted(info) => match &mut s.live {
                Some(live) if live.model.session_id == info.session_id => live.info = Some(info),
                _ => s.live = Some(LiveSession { info: Some(info), model: TimelineModel::new(), last_status: None }),
            },
            Action::Event(event) => {
                let same = s.live.as_ref().is_some_and(|l| l.model.session_id.is_empty() || l.model.session_id == event.session_id);
                if !same {
                    s.live = Some(LiveSession { info: None, model: TimelineModel::new(), last_status: None });
                }
                if let Some(live) = &mut s.live {
                    live.model.apply(&event);
                }
            }
            Action::Approval(req) => s.approval = Some(req),
            Action::ApprovalClosed(id) => {
                if s.approval.as_ref().is_some_and(|a| a.request_id == id) {
                    s.approval = None;
                }
            }
            Action::Sending => {
                s.running = true;
                s.cancelling = false;
                s.history = None;
                s.banner = None;
            }
            Action::Cancelling => s.cancelling = true,
            Action::SendFailed(text) => {
                s.running = false;
                s.cancelling = false;
                s.banner = Some(Banner { kind: BannerKind::Error, text });
            }
            Action::TurnFinished(done) => {
                s.running = false;
                s.cancelling = false;
                s.approval = None;
                if let Some(live) = &mut s.live {
                    live.last_status = Some(done.status);
                }
                if let Some(banner) = Self::banner_for(&done) {
                    s.banner = Some(banner);
                }
            }
            Action::HistoryLoaded(dto) => {
                s.history = Some(HistoryView { model: TimelineModel::fold_history(&dto.events), issues: dto.log_issues });
            }
            Action::CloseHistory => s.history = None,
            Action::NewSession => {
                s.live = None;
                s.history = None;
                s.running = false;
                s.cancelling = false;
                s.approval = None;
                s.banner = None;
            }
            Action::Restored { info, events, running } => {
                let mut model = TimelineModel::new();
                events.iter().for_each(|e| model.apply(e));
                s.live = Some(LiveSession { info: Some(info), model, last_status: None });
                s.running = running;
            }
            Action::Error(text) => s.banner = Some(Banner { kind: BannerKind::Error, text }),
            Action::Notice(text) => s.banner = Some(Banner { kind: BannerKind::Info, text }),
            Action::DismissBanner => s.banner = None,
            Action::SettingsOpen(open) => s.settings_open = open,
        }
        Rc::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightning_protocol::dto::PermissionMode;
    use lightning_protocol::fixtures;
    use lightning_protocol::view::RunStatus;

    fn settings(key_present: bool, last: Option<&str>) -> SettingsDto {
        SettingsDto {
            model: "m".into(),
            default_model: "m".into(),
            known_models: vec!["m".into()],
            permission_mode: PermissionMode::Ask,
            recent_workspaces: vec![],
            last_workspace: last.map(String::from),
            key_present,
        }
    }

    fn info(id: &str) -> SessionInfoDto {
        SessionInfoDto {
            session_id: id.into(),
            workspace: "C:\\w".into(),
            provider: "gemini".into(),
            model: "m".into(),
            permission_mode: PermissionMode::Ask,
        }
    }

    fn apply(state: AppState, actions: Vec<Action>) -> AppState {
        let mut rc = Rc::new(state);
        for a in actions {
            rc = rc.reduce(a);
        }
        (*rc).clone()
    }

    fn done(status: SessionStatus, error: Option<&str>) -> TurnFinishedDto {
        TurnFinishedDto { session_id: fixtures::SESSION_ID.into(), status, error: error.map(String::from) }
    }

    #[test]
    fn settings_supply_the_last_workspace_and_gate_sending() {
        let s = apply(AppState::default(), vec![Action::SettingsLoaded(settings(true, Some("C:\\w")))]);
        assert_eq!(s.workspace.as_deref(), Some("C:\\w"));
        assert!(s.can_send());

        let no_key = apply(AppState::default(), vec![Action::SettingsLoaded(settings(false, Some("C:\\w")))]);
        assert!(!no_key.can_send());
        let no_ws = apply(AppState::default(), vec![Action::SettingsLoaded(settings(true, None))]);
        assert!(!no_ws.can_send());
    }

    #[test]
    fn events_before_session_started_are_kept_and_info_attaches_later() {
        let mut actions = vec![Action::SettingsLoaded(settings(true, Some("C:\\w"))), Action::Sending];
        actions.extend(fixtures::happy_path().into_iter().take(3).map(Action::Event));
        actions.push(Action::SessionStarted(info(fixtures::SESSION_ID)));
        let s = apply(AppState::default(), actions);
        let live = s.live.unwrap();
        assert_eq!(live.info.unwrap().session_id, fixtures::SESSION_ID);
        assert_eq!(live.model.session_id, fixtures::SESSION_ID);
        assert_eq!(live.model.items.len(), 1); // the user message
        assert!(s.running);
    }

    #[test]
    fn a_full_turn_ends_idle_and_can_continue() {
        let mut actions = vec![Action::SettingsLoaded(settings(true, Some("C:\\w"))), Action::Sending];
        actions.extend(fixtures::happy_path().into_iter().filter(|e| !matches!(e.kind, lightning_protocol::EventKind::SessionEnded { .. })).map(Action::Event));
        actions.push(Action::TurnFinished(done(SessionStatus::Completed, None)));
        let s = apply(AppState::default(), actions);
        assert!(!s.running);
        assert!(s.can_send());
        assert!(s.banner.is_none());
        assert_eq!(s.live.unwrap().model.tool_calls, 2);
    }

    #[test]
    fn a_failed_turn_blocks_further_sending_and_explains_why() {
        let s = apply(
            AppState::default(),
            vec![
                Action::SettingsLoaded(settings(true, Some("C:\\w"))),
                Action::Sending,
                Action::Event(fixtures::happy_path().remove(0)),
                Action::TurnFinished(done(SessionStatus::LlmError, Some("HTTP 500"))),
            ],
        );
        assert!(!s.can_send());
        assert!(s.banner.as_ref().unwrap().text.contains("HTTP 500"));
        // New session clears everything and re-enables sending.
        let s = apply(s, vec![Action::NewSession]);
        assert!(s.can_send());
        assert!(s.banner.is_none() && s.live.is_none());

        let cancelled = apply(AppState::default(), vec![Action::TurnFinished(done(SessionStatus::Cancelled, None))]);
        assert!(cancelled.banner.is_none());
    }

    #[test]
    fn approvals_open_and_close_by_id_and_end_with_the_turn() {
        let req = ApprovalRequestDto { request_id: "r1".into(), session_id: "s".into(), tool: "write_file".into(), summary: "write a.txt".into() };
        let s = apply(AppState::default(), vec![Action::Approval(req.clone())]);
        assert_eq!(s.approval, Some(req.clone()));
        let s = apply(s, vec![Action::ApprovalClosed("other".into())]);
        assert!(s.approval.is_some());
        let s = apply(s, vec![Action::ApprovalClosed("r1".into())]);
        assert!(s.approval.is_none());
        let s = apply(s, vec![Action::Approval(req), Action::TurnFinished(done(SessionStatus::Cancelled, None))]);
        assert!(s.approval.is_none());
    }

    #[test]
    fn history_blocks_sending_until_closed_and_marks_crashed_logs() {
        let dto = LoadedSessionDto {
            summary: Summary::from_events(fixtures::SESSION_ID, &fixtures::unfinished()),
            events: fixtures::unfinished(),
            log_issues: LogIssuesDto { truncated_last_line: true, bad_lines: vec![] },
        };
        let s = apply(AppState::default(), vec![Action::SettingsLoaded(settings(true, Some("C:\\w"))), Action::HistoryLoaded(dto)]);
        let h = s.history.clone().unwrap();
        assert_eq!(h.model.status, RunStatus::Unfinished);
        assert!(h.issues.truncated_last_line);
        assert!(!s.can_send());
        assert!(apply(s, vec![Action::CloseHistory]).can_send());
    }

    #[test]
    fn changing_workspace_resets_session_state_but_choosing_the_same_one_keeps_it() {
        let mut actions = vec![Action::WorkspaceChosen("A".into()), Action::SessionStarted(info("20261010T000000Z-aaaa"))];
        actions.push(Action::Sending);
        let s = apply(AppState::default(), actions);
        assert!(s.live.is_some() && s.running);

        let same = apply(s.clone(), vec![Action::WorkspaceChosen("A".into())]);
        assert!(same.live.is_some() && same.running);
        let other = apply(s, vec![Action::WorkspaceChosen("B".into())]);
        assert!(other.live.is_none() && !other.running);
        assert_eq!(other.workspace.as_deref(), Some("B"));
    }

    #[test]
    fn restored_sessions_rebuild_the_timeline_and_the_running_flag() {
        let events: Vec<Event> = fixtures::happy_path().into_iter().take(8).collect();
        let s = apply(AppState::default(), vec![Action::Restored { info: info(fixtures::SESSION_ID), events, running: true }]);
        assert!(s.running);
        assert!(s.live.unwrap().model.tool_calls >= 1);
    }

    #[test]
    fn a_missing_key_gets_a_banner_unless_another_banner_is_showing() {
        let s = apply(AppState::default(), vec![Action::SettingsLoaded(settings(false, None))]);
        assert!(s.missing_key_banner().is_some());
        let with_key = apply(AppState::default(), vec![Action::SettingsLoaded(settings(true, None))]);
        assert!(with_key.missing_key_banner().is_none());
        assert!(AppState::default().missing_key_banner().is_none(), "no banner before settings have loaded");
        let busy = apply(s, vec![Action::Error("boom".into())]);
        assert!(busy.missing_key_banner().is_none());
    }

    #[test]
    fn a_failed_send_stops_the_spinner_and_shows_the_reason() {
        let s = apply(AppState::default(), vec![Action::Sending, Action::SendFailed("No API key yet.".into())]);
        assert!(!s.running);
        assert_eq!(s.banner.unwrap().text, "No API key yet.");
    }

    #[test]
    fn banners_can_be_set_and_dismissed() {
        let s = apply(AppState::default(), vec![Action::Error("boom".into())]);
        assert_eq!(s.banner.as_ref().map(|b| b.kind.clone()), Some(BannerKind::Error));
        assert!(apply(s, vec![Action::DismissBanner]).banner.is_none());
    }
}
