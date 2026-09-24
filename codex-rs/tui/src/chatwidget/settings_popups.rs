//! Settings-adjacent popup surfaces for `ChatWidget`.
//!
//! This keeps theme and experimental-feature UI out of the main
//! orchestration module without changing their event wiring.

use super::*;
use crate::render::renderable::ColumnRenderable;

impl ChatWidget {
    pub(super) fn open_theme_picker(&mut self) {
        let codex_home = codex_utils_home_dir::find_codex_home().ok();
        let params = crate::theme_picker::build_theme_picker_params(
            self.local_settings.tui.theme.as_deref(),
            codex_home.as_deref(),
            self.last_rendered_width.get(),
        );
        self.bottom_pane.show_selection_view(params);
    }

    /// Open a popup to switch the active session to another configured model
    /// provider (built-ins plus `[model_providers]` entries).
    ///
    /// Known limitation: the /model picker's catalog is NOT refreshed after a
    /// provider switch; it still lists the launch provider's models. Pick
    /// models for the new provider via config profiles or by model name.
    pub(crate) fn open_provider_popup(&mut self) {
        if !self.is_session_configured() {
            self.add_info_message(
                "Provider selection is disabled until startup completes.".to_string(),
                /*hint*/ None,
            );
            return;
        }

        let current_provider_id = self.config.model_provider_id.clone();
        let mut providers: Vec<(String, String)> = self
            .config
            .model_providers
            .iter()
            .map(|(id, info)| (id.clone(), info.name.clone()))
            .collect();
        providers.sort_by(|(left, _), (right, _)| left.cmp(right));

        let items: Vec<SelectionItem> = providers
            .into_iter()
            .map(|(id, name)| {
                let is_current = id == current_provider_id;
                let selected_id = id.clone();
                let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                    tx.send(AppEvent::UpdateModelProvider(selected_id.clone()));
                })];
                SelectionItem {
                    name,
                    description: Some(id),
                    is_current,
                    actions,
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        let mut header = ColumnRenderable::new();
        header.push(Line::from("Select Model Provider".bold()));
        header.push(Line::from(
            "Switch this session to another configured provider.".dim(),
        ));

        self.bottom_pane.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    /// Open a popup to request a cyber access program (Daybreak) on upcoming
    /// turns. The selection is forwarded on ChatGPT-authenticated turns only;
    /// the server owns authorization and rejects ungranted programs.
    pub(crate) fn open_daybreak_popup(&mut self) {
        use codex_app_server_protocol::CyberAccessProgram;

        if !self.is_session_configured() {
            self.add_info_message(
                "Daybreak selection is disabled until startup completes.".to_string(),
                /*hint*/ None,
            );
            return;
        }

        let current = self.cyber_access_program();
        let choices: [(Option<CyberAccessProgram>, &str, &str); 4] = [
            (None, "Automatic", "let the server choose (default)"),
            (
                Some(CyberAccessProgram::Standard),
                "Standard",
                "explicitly request the standard program",
            ),
            (
                Some(CyberAccessProgram::DaybreakBlue),
                "Daybreak Blue",
                "defensive cyber program (requires an active grant)",
            ),
            (
                Some(CyberAccessProgram::DaybreakRed),
                "Daybreak Red",
                "offensive cyber program (requires an active grant)",
            ),
        ];
        let items: Vec<SelectionItem> = choices
            .into_iter()
            .map(|(program, name, description)| {
                let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                    tx.send(AppEvent::UpdateCyberAccessProgram(program));
                })];
                SelectionItem {
                    name: name.to_string(),
                    description: Some(description.to_string()),
                    is_current: current == program,
                    actions,
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        let mut header = ColumnRenderable::new();
        header.push(Line::from("Select Cyber Access Program".bold()));
        header.push(Line::from(
            "Applies to upcoming turns on ChatGPT auth; ignored on API-key providers.".dim(),
        ));

        self.bottom_pane.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn open_experimental_popup(&mut self) {
        let Some(thread_id) = self.thread_id() else {
            self.add_info_message(
                "Experimental features are unavailable until startup completes.".to_string(),
                /*hint*/ None,
            );
            return;
        };
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        self.app_event_tx.send(AppEvent::FetchExperimentalFeatures {
            thread_id,
            response_tx,
        });
        let view = ExperimentalFeaturesView::new(
            Vec::new(),
            thread_id,
            Some(response_rx),
            self.app_event_tx.clone(),
            self.bottom_pane.list_keymap(),
        );
        self.bottom_pane.show_view(Box::new(view));
    }
}
