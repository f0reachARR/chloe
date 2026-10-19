//! 規則のスコープを選び、そのスコープで効く規則を見て変える Rules タブ。

use chloe_controller::runner::RuleOrigin;
use chloe_policy::{Action as RuleAction, PathRuleConfig};
use chloe_secrets::Owner;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::{Block, ListItem, Paragraph, Wrap};
use terminput::{KeyCode, KeyEvent};

use super::app::{Action, App, Modal, Purpose, Tab, owner_kind, render_list, tab_index};
use super::policy_form;
use crate::ops::{RuleScope, RulesView, ScopeEntry, owner_arg};

impl App {
    /// Rules タブで選んでいるスコープ。
    pub(super) fn selected_scope(&self) -> Option<&ScopeEntry> {
        self.scopes.get(self.selected[tab_index(Tab::Rules)])
    }

    /// 選んでいるスコープで効く規則を、まだ読んでいなければ読む。
    pub(super) fn show_selected_rules(&self) -> Vec<Action> {
        match self.selected_scope() {
            Some(entry) if self.rules.as_ref().map(|r| &r.scope) != Some(&entry.scope) => {
                vec![Action::ShowRules(entry.scope.clone())]
            }
            _ => vec![],
        }
    }

    /// 読んである、選んでいるスコープの規則。
    fn selected_rules(&self) -> Option<&RulesView> {
        let entry = self.selected_scope()?;
        self.rules.as_ref().filter(|r| r.scope == entry.scope)
    }

    /// Rules タブでキーを受ける。
    pub(super) fn handle_rules_key(&mut self, key: KeyEvent) -> Vec<Action> {
        match key.code {
            KeyCode::Enter | KeyCode::Char('e') => {
                let Some(view) = self.selected_rules() else {
                    return self.show_selected_rules();
                };
                if !view.editable {
                    self.status = Some((
                        format!("you cannot change the rules of {}", view.scope.label()),
                        true,
                    ));
                    return vec![];
                }
                self.modals.push(Modal::Form {
                    form: policy_form::rules_form(&view.scope.label(), &view.own),
                    purpose: Purpose::Rules(view.scope.clone()),
                    saving: false,
                });
            }
            KeyCode::Char('d') => {
                if let Some(view) = self.selected_rules()
                    && view.editable
                    && !view.own.is_empty()
                {
                    self.modals.push(Modal::Confirm {
                        message: format!("Delete the rules of {}?", view.scope.label()),
                        action: Action::ClearRules(view.scope.clone()),
                    });
                }
            }
            _ => {}
        }
        vec![]
    }

    /// 読んだスコープの一覧を入れる。選んでいたスコープは選んだままにする。
    pub(super) fn apply_rule_scopes(&mut self, scopes: Vec<ScopeEntry>) -> Vec<Action> {
        let current = self.selected_scope().map(|e| e.scope.clone());
        let index = current
            .and_then(|c| scopes.iter().position(|e| e.scope == c))
            .unwrap_or(0);
        self.scopes = scopes;
        self.selected[tab_index(Tab::Rules)] = index;
        // 一覧を読み直したら、規則も読み直す (変えられるかが変わることがある)。
        self.rules = None;
        self.show_selected_rules()
    }

    /// 読んだ規則を、選んでいるスコープのものなら入れる。
    pub(super) fn apply_rules(&mut self, view: RulesView) {
        if self.selected_scope().map(|e| &e.scope) == Some(&view.scope) {
            self.rules = Some(view);
        }
    }

    /// Rules タブ。左にスコープの一覧、右に選んでいるスコープで効く規則を評価する順に出す。
    pub(super) fn render_rules(&self, frame: &mut Frame, area: Rect) {
        let [list_area, rules_area] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(area);
        let items: Vec<ListItem> = self
            .scopes
            .iter()
            .map(|entry| {
                let (depth, kind) = scope_shape(&entry.scope);
                let access = if entry.editable { "edit" } else { "view" };
                let name = format!("{}{}", "  ".repeat(depth), entry.scope.label());
                ListItem::new(format!("{name:<26} {kind:<8} {access}"))
            })
            .collect();
        render_list(frame, list_area, items, self.selection());
        let block = Block::bordered().title(match self.selected_scope() {
            Some(entry) => format!(" Rules applied to {} ", entry.scope.label()),
            None => " Rules ".to_string(),
        });
        let inner = block.inner(rules_area);
        frame.render_widget(block, rules_area);
        let Some(view) = self.selected_rules() else {
            frame.render_widget(Paragraph::new("…".dark_gray()), inner);
            return;
        };
        let mut lines: Vec<Line> = view
            .rules
            .iter()
            .map(|(origin, rule)| {
                let own = is_own(&view.scope, origin);
                let text = format!(
                    "{} {:<14} {}",
                    if own { "▶" } else { " " },
                    origin_label(origin),
                    describe_rule(rule)
                );
                if own {
                    Line::from(text.bold())
                } else {
                    Line::from(text)
                }
            })
            .collect();
        lines.push(Line::from("  anything else is blocked".dark_gray()));
        if !view.editable {
            lines.push(Line::default());
            lines.push(Line::from("  (view only)".dark_gray()));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}

/// スコープの一覧での字下げと種類。ワークスペースは持ち主の下に、team は Organization の下に置く。
fn scope_shape(scope: &RuleScope) -> (usize, &'static str) {
    let depth = |owner: &Owner| usize::from(matches!(owner, Owner::Team { .. }));
    match scope {
        RuleScope::Gateway => (0, "config"),
        RuleScope::Owner(owner) => (depth(owner), owner_kind(owner)),
        RuleScope::Workspace(ws) => (depth(&ws.owner) + 1, "ws"),
    }
}

/// 規則がスコープ自身のものか。
fn is_own(scope: &RuleScope, origin: &RuleOrigin) -> bool {
    match (scope, origin) {
        (RuleScope::Gateway, RuleOrigin::Gateway) => true,
        (RuleScope::Owner(owner), RuleOrigin::Owner(from)) => owner == from,
        (RuleScope::Workspace(_), RuleOrigin::Workspace) => true,
        _ => false,
    }
}

fn origin_label(origin: &RuleOrigin) -> String {
    match origin {
        RuleOrigin::Gateway => "gateway".into(),
        RuleOrigin::Owner(owner) => owner_arg(owner),
        RuleOrigin::Workspace => "workspace".into(),
        RuleOrigin::Secret => "(secrets)".into(),
    }
}

fn describe_rule(rule: &PathRuleConfig) -> String {
    let action = match rule.action {
        RuleAction::Allow => "allow",
        RuleAction::Deny => "deny",
        RuleAction::Passthrough => "pass",
    };
    let mut text = format!("{action:<5} {}", rule.hosts.join(", "));
    if !rule.methods.is_empty() {
        text.push_str(&format!(" {}", rule.methods.join(",")));
    }
    if !rule.paths.is_empty() {
        text.push_str(&format!(" {}", rule.paths.join(", ")));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::super::app::Outcome;
    use super::super::app::tests::{app, form_mut, screen};
    use super::super::form::tests::{ctrl, key, type_text};
    use super::*;

    fn rule(host: &str, action: RuleAction) -> PathRuleConfig {
        PathRuleConfig {
            name: None,
            hosts: vec![host.into()],
            methods: vec![],
            paths: vec![],
            action,
        }
    }

    #[test]
    fn edits_rules_of_the_selected_scope() {
        let mut app = app();
        app.handle_key(key(KeyCode::Char('3')));
        // 選んだスコープで効く規則を読む。
        for _ in 0..4 {
            app.handle_key(key(KeyCode::Down));
        }
        let actions = app.handle_key(key(KeyCode::Up));
        let [Action::ShowRules(scope @ RuleScope::Owner(Owner::Org(_)))] = &actions[..] else {
            panic!("expected the rules of acme");
        };
        let acme = Owner::Org("acme".into());
        app.apply(Outcome::Rules(RulesView {
            scope: scope.clone(),
            editable: false,
            rules: vec![
                (RuleOrigin::Gateway, rule("github.com", RuleAction::Allow)),
                (
                    RuleOrigin::Owner(acme.clone()),
                    rule("example.org", RuleAction::Allow),
                ),
            ],
            own: vec![rule("example.org", RuleAction::Allow)],
        }));
        let text = screen(&app);
        assert!(text.contains("Rules applied to acme"));
        assert!(text.contains("▶ acme"));
        assert!(text.contains("anything else is blocked"));
        // 変えられないスコープでは、フォームを開かない。
        assert!(app.handle_key(key(KeyCode::Enter)).is_empty());
        assert!(app.modals.is_empty());
        assert!(screen(&app).contains("you cannot change the rules of acme"));

        // ワークスペースのスコープで、規則を足して保存する。
        let actions = app.handle_key(key(KeyCode::Down));
        let [Action::ShowRules(scope)] = &actions[..] else {
            panic!("expected the rules of acme/web");
        };
        app.apply(Outcome::Rules(RulesView {
            scope: scope.clone(),
            editable: true,
            rules: vec![],
            own: vec![],
        }));
        app.handle_key(key(KeyCode::Enter));
        let form = form_mut(&mut app);
        while form.fields[form.focus].key != "rules" {
            form.handle_key(key(KeyCode::Down));
        }
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Char('a')));
        form_mut(&mut app).handle_key(key(KeyCode::Tab));
        type_text(form_mut(&mut app), "pypi.org");
        app.handle_key(ctrl('s'));
        app.handle_key(key(KeyCode::Esc));
        let actions = app.handle_key(ctrl('s'));
        let [Action::SaveRules { scope, rules }] = &actions[..] else {
            panic!("expected a save");
        };
        assert!(matches!(scope, RuleScope::Workspace(ws) if ws.display_name() == "acme/web"));
        assert_eq!(rules, &[rule("pypi.org", RuleAction::Allow)]);
    }

    #[test]
    fn opens_the_rules_of_a_workspace() {
        let mut app = app();
        let actions = app.handle_key(key(KeyCode::Char('u')));
        assert!(matches!(
            &actions[..],
            [Action::ShowRules(RuleScope::Workspace(ws))] if ws.name == "default"
        ));
        assert_eq!(app.current(), Tab::Rules);
        // 古い結果 (ほかのスコープのもの) は使わない。
        app.apply(Outcome::Rules(RulesView {
            scope: RuleScope::Gateway,
            editable: false,
            rules: vec![(RuleOrigin::Gateway, rule("github.com", RuleAction::Allow))],
            own: vec![],
        }));
        assert!(!screen(&app).contains("github.com"));
    }
}
