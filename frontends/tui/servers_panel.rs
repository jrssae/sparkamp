//! The Servers panel in the Media Library (`S` on the Files tab): list the
//! configured servers, add one through a short sequence of prompts, remove
//! one, test one.
//!
//! The key handling is a state machine that only returns what should happen
//! ([`PanelAction`]); the app does the saving, the Keychain and the restart.
//! That keeps the panel testable without touching the real config file or
//! the user's Keychain.

use crossterm::event::KeyCode;
use sparkamp::config::ServerConfig;

/// The prompts of the Add form, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormStep {
    Name,
    LanUrl,
    RemoteUrl,
    Username,
    Password,
}

impl FormStep {
    pub fn label(self) -> &'static str {
        match self {
            FormStep::Name => "Name, a nickname for this server",
            FormStep::LanUrl => "Home address, on your home network, http:// or https:// (Enter to skip)",
            FormStep::RemoteUrl => "Remote address, from anywhere, https:// only (Enter to skip)",
            FormStep::Username => "Username, your account name on the server",
            FormStep::Password => "Password, your password on the server",
        }
    }
}

/// The Add form's answers so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerForm {
    pub step: FormStep,
    pub name: String,
    pub lan_url: String,
    pub remote_url: String,
    pub username: String,
    pub password: String,
}

/// The panel's state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServersPanel {
    pub selected: usize,
    pub form: Option<ServerForm>,
    /// Waiting for `y` to remove the selected server.
    pub confirm_remove: bool,
    /// The last result or problem, shown under the list.
    pub message: Option<String>,
}

/// What the app should do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelAction {
    Nothing,
    Close,
    Add { config: ServerConfig, password: String },
    Remove(String),
    Test(String),
}

impl ServersPanel {
    /// Handle one key. `servers` is the configured list, for selection.
    pub fn key(&mut self, code: KeyCode, servers: &[ServerConfig]) -> PanelAction {
        if let Some(form) = &mut self.form {
            match code {
                KeyCode::Esc => {
                    self.form = None;
                    self.message = None;
                }
                KeyCode::Backspace => {
                    field_mut(form).pop();
                }
                KeyCode::Char(c) => field_mut(form).push(c),
                KeyCode::Enter => return self.advance(),
                _ => {}
            }
            return PanelAction::Nothing;
        }
        if self.confirm_remove {
            self.confirm_remove = false;
            self.message = None;
            if code == KeyCode::Char('y') {
                if let Some(s) = servers.get(self.selected) {
                    return PanelAction::Remove(s.id.clone());
                }
            }
            return PanelAction::Nothing;
        }
        match code {
            KeyCode::Esc => return PanelAction::Close,
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                if self.selected + 1 < servers.len() {
                    self.selected += 1;
                }
            }
            KeyCode::Char('a') => {
                self.message = None;
                self.form = Some(ServerForm {
                    step: FormStep::Name,
                    name: String::new(),
                    lan_url: String::new(),
                    remote_url: String::new(),
                    username: String::new(),
                    password: String::new(),
                });
            }
            KeyCode::Char('d') => {
                if let Some(s) = servers.get(self.selected) {
                    self.confirm_remove = true;
                    self.message = Some(format!(
                        "Remove {}? Its cached catalog goes too; your files stay. y/n",
                        s.name
                    ));
                }
            }
            KeyCode::Char('t') => {
                if let Some(s) = servers.get(self.selected) {
                    self.message = Some(format!("Testing {}…", s.name));
                    return PanelAction::Test(s.id.clone());
                }
            }
            _ => {}
        }
        PanelAction::Nothing
    }

    /// Enter on the form: check this answer, then move on, or finish.
    fn advance(&mut self) -> PanelAction {
        let Some(form) = self.form.as_mut() else { return PanelAction::Nothing };
        let problem = match form.step {
            FormStep::Name if form.name.trim().is_empty() => Some("A name is needed."),
            FormStep::LanUrl
                if !form.lan_url.trim().is_empty()
                    && !(form.lan_url.trim().starts_with("http://")
                        || form.lan_url.trim().starts_with("https://")) =>
            {
                Some("The LAN URL must start with http:// or https://.")
            }
            FormStep::RemoteUrl
                if !form.remote_url.trim().is_empty()
                    && !form.remote_url.trim().starts_with("https://") =>
            {
                Some("The remote URL must use https: plain HTTP is only allowed on the LAN URL.")
            }
            FormStep::RemoteUrl
                if form.lan_url.trim().is_empty() && form.remote_url.trim().is_empty() =>
            {
                Some("Give at least one URL.")
            }
            FormStep::Username if form.username.trim().is_empty() => Some("A username is needed."),
            _ => None,
        };
        if let Some(problem) = problem {
            self.message = Some(problem.to_string());
            return PanelAction::Nothing;
        }
        // Plain HTTP across the internet: allowed, but said.
        self.message = match form.step {
            FormStep::LanUrl => {
                sparkamp::servers::validate::home_address_warning(&form.lan_url).map(|w| format!("⚠ {w}"))
            }
            _ => None,
        };
        form.step = match form.step {
            FormStep::Name => FormStep::LanUrl,
            FormStep::LanUrl => FormStep::RemoteUrl,
            FormStep::RemoteUrl => FormStep::Username,
            FormStep::Username => FormStep::Password,
            FormStep::Password => {
                let url = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
                let mut config = ServerConfig::new(form.name.trim());
                config.lan_url = url(&form.lan_url);
                config.remote_url = url(&form.remote_url);
                config.username = form.username.trim().to_string();
                let password = form.password.clone();
                self.message = Some(format!("Added {}.", config.name));
                self.form = None;
                return PanelAction::Add { config, password };
            }
        };
        PanelAction::Nothing
    }
}

fn field_mut(form: &mut ServerForm) -> &mut String {
    match form.step {
        FormStep::Name => &mut form.name,
        FormStep::LanUrl => &mut form.lan_url,
        FormStep::RemoteUrl => &mut form.remote_url,
        FormStep::Username => &mut form.username,
        FormStep::Password => &mut form.password,
    }
}

/// The text shown for the form's current field: the password as dots.
pub fn shown_value(form: &ServerForm) -> String {
    match form.step {
        FormStep::Password => "•".repeat(form.password.chars().count()),
        FormStep::Name => form.name.clone(),
        FormStep::LanUrl => form.lan_url.clone(),
        FormStep::RemoteUrl => form.remote_url.clone(),
        FormStep::Username => form.username.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(panel: &mut ServersPanel, text: &str, servers: &[ServerConfig]) -> PanelAction {
        for c in text.chars() {
            panel.key(KeyCode::Char(c), servers);
        }
        panel.key(KeyCode::Enter, servers)
    }

    fn server(id: &str, name: &str) -> ServerConfig {
        ServerConfig { id: id.into(), name: name.into(), ..ServerConfig::default() }
    }

    #[test]
    fn adding_a_server_walks_through_the_prompts() {
        let mut p = ServersPanel::default();
        assert_eq!(p.key(KeyCode::Char('a'), &[]), PanelAction::Nothing);
        assert_eq!(p.form.as_ref().unwrap().step, FormStep::Name);
        typed(&mut p, "oscar", &[]);
        typed(&mut p, "http://oscar.local:4533", &[]);
        typed(&mut p, "https://music.example.com", &[]);
        typed(&mut p, "me", &[]);
        let action = typed(&mut p, "sesame", &[]);
        match action {
            PanelAction::Add { config, password } => {
                assert_eq!(config.name, "oscar");
                assert_eq!(config.lan_url.as_deref(), Some("http://oscar.local:4533"));
                assert_eq!(config.remote_url.as_deref(), Some("https://music.example.com"));
                assert_eq!(config.username, "me");
                assert!(config.enabled);
                assert_eq!(config.id.len(), 36, "a fresh id");
                assert_eq!(password, "sesame");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(p.form, None);
    }

    /// Plain HTTP across the internet is allowed, but said out loud as soon
    /// as the home address is given; one at home passes quietly.
    #[test]
    fn a_plain_http_home_address_off_the_home_network_is_warned_about() {
        let mut p = ServersPanel::default();
        p.key(KeyCode::Char('a'), &[]);
        typed(&mut p, "oscar", &[]);
        typed(&mut p, "http://music.example.com", &[]);
        assert_eq!(p.form.as_ref().unwrap().step, FormStep::RemoteUrl, "a warning, not a refusal");
        assert!(p.message.as_deref().unwrap_or("").contains("plain HTTP"), "{:?}", p.message);

        let mut p = ServersPanel::default();
        p.key(KeyCode::Char('a'), &[]);
        typed(&mut p, "oscar", &[]);
        typed(&mut p, "http://oscar.local:4533", &[]);
        assert_eq!(p.message, None);
    }

    #[test]
    fn an_empty_url_is_allowed_but_not_both() {
        let mut p = ServersPanel::default();
        p.key(KeyCode::Char('a'), &[]);
        typed(&mut p, "oscar", &[]);
        typed(&mut p, "", &[]);
        typed(&mut p, "", &[]);
        assert_eq!(p.form.as_ref().unwrap().step, FormStep::RemoteUrl, "needs one URL");
        assert!(p.message.as_deref().unwrap_or("").contains("URL"));
    }

    #[test]
    fn a_plain_http_remote_url_is_refused() {
        let mut p = ServersPanel::default();
        p.key(KeyCode::Char('a'), &[]);
        typed(&mut p, "oscar", &[]);
        typed(&mut p, "", &[]);
        typed(&mut p, "http://music.example.com", &[]);
        assert_eq!(p.form.as_ref().unwrap().step, FormStep::RemoteUrl);
        assert!(p.message.as_deref().unwrap_or("").contains("https"), "{:?}", p.message);
    }

    #[test]
    fn backspace_edits_and_escape_abandons_the_form() {
        let mut p = ServersPanel::default();
        p.key(KeyCode::Char('a'), &[]);
        typed_no_enter(&mut p, "oscarr");
        p.key(KeyCode::Backspace, &[]);
        assert_eq!(p.form.as_ref().unwrap().name, "oscar");
        p.key(KeyCode::Esc, &[]);
        assert_eq!(p.form, None);
        assert_eq!(p.key(KeyCode::Esc, &[]), PanelAction::Close, "a second Esc closes the panel");
    }

    fn typed_no_enter(panel: &mut ServersPanel, text: &str) {
        for c in text.chars() {
            panel.key(KeyCode::Char(c), &[]);
        }
    }

    #[test]
    fn the_password_is_shown_as_dots() {
        let form = ServerForm {
            step: FormStep::Password,
            name: "oscar".into(),
            lan_url: String::new(),
            remote_url: String::new(),
            username: "me".into(),
            password: "sesame".into(),
        };
        assert_eq!(shown_value(&form), "••••••");
        let form = ServerForm { step: FormStep::Username, ..form };
        assert_eq!(shown_value(&form), "me");
    }

    #[test]
    fn removing_asks_first() {
        let servers = [server("a", "oscar"), server("b", "server2")];
        let mut p = ServersPanel::default();
        p.key(KeyCode::Down, &servers);
        assert_eq!(p.key(KeyCode::Char('d'), &servers), PanelAction::Nothing);
        assert!(p.confirm_remove);
        assert_eq!(p.key(KeyCode::Char('n'), &servers), PanelAction::Nothing);
        assert!(!p.confirm_remove);
        p.key(KeyCode::Char('d'), &servers);
        assert_eq!(p.key(KeyCode::Char('y'), &servers), PanelAction::Remove("b".into()));
    }

    #[test]
    fn t_tests_the_selected_server() {
        let servers = [server("a", "oscar")];
        let mut p = ServersPanel::default();
        assert_eq!(p.key(KeyCode::Char('t'), &servers), PanelAction::Test("a".into()));
        assert_eq!(p.key(KeyCode::Char('t'), &[]), PanelAction::Nothing, "nothing to test");
    }
}
