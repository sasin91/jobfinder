//! Billing failures reported during the current app session.

use tokio::sync::watch;

use super::LlmResponse;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Provider {
    Openai,
    Anthropic,
}

impl Provider {
    const ALL: [Self; 2] = [Self::Openai, Self::Anthropic];

    pub fn name(self) -> &'static str {
        match self {
            Self::Openai => "OpenAI",
            Self::Anthropic => "Anthropic",
        }
    }

    pub fn url(self) -> &'static str {
        match self {
            Self::Openai => "https://platform.openai.com/settings/organization/billing/overview",
            Self::Anthropic => "https://platform.claude.com/settings/billing",
        }
    }

    pub(super) fn blocked(self, answer: &LlmResponse) -> bool {
        if !(400..500).contains(&answer.status) {
            return false;
        }
        let code = answer.error_code();
        let detail = answer.error_message().to_ascii_lowercase();
        match self {
            Self::Openai => {
                matches!(code.as_str(), "insufficient_quota" | "credit_balance_exhausted")
                    || detail.contains("no credits remaining")
            }
            Self::Anthropic => detail.starts_with("your credit balance is too low"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Billing(watch::Sender<[bool; 2]>);

impl Billing {
    pub(super) fn observe(&self, provider: Provider, blocked: bool, accepted: bool) {
        if blocked || accepted {
            self.set_blocked(provider, blocked);
        }
    }

    pub fn dismiss(&self, provider: Provider) {
        self.set_blocked(provider, false);
    }

    fn set_blocked(&self, provider: Provider, blocked: bool) {
        self.0.send_if_modified(|providers| {
            let active = &mut providers[provider as usize];
            let changed = *active != blocked;
            *active = blocked;
            changed
        });
    }

    pub fn active(&self) -> impl Iterator<Item = Provider> {
        let active = *self.0.borrow();
        Provider::ALL
            .into_iter()
            .filter(move |provider| active[*provider as usize])
    }

    pub fn changes(&self) -> watch::Receiver<[bool; 2]> {
        self.0.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn billing_blocks_survive_unrelated_failures_and_clear_independently() {
        let billing = Billing::default();
        let mut changes = billing.changes();
        let exhausted = LlmResponse::new(
            429,
            vec![],
            r#"{"error":{"message":"You have no credits remaining.","code":"rate_limit_exceeded"}}"#,
        );
        let throttled = LlmResponse::new(
            429,
            vec![],
            r#"{"error":{"message":"Rate limit reached.","code":"rate_limit_exceeded"}}"#,
        );
        let low_balance = LlmResponse::new(
            400,
            vec![],
            r#"{"error":{"message":"Your credit balance is too low to access the Anthropic API."}}"#,
        );
        let openai = Provider::Openai;
        let anthropic = Provider::Anthropic;
        assert!(openai.blocked(&exhausted));
        assert!(!openai.blocked(&throttled));
        assert!(anthropic.blocked(&low_balance));
        billing.observe(openai, openai.blocked(&exhausted), false);
        assert!(changes.has_changed().unwrap());
        changes.borrow_and_update();
        billing.observe(openai, openai.blocked(&throttled), false);
        assert!(!changes.has_changed().unwrap());
        billing.observe(anthropic, anthropic.blocked(&low_balance), false);
        assert_eq!(billing.active().collect::<Vec<_>>(), [openai, anthropic]);
        billing.observe(openai, false, true);
        assert_eq!(billing.active().collect::<Vec<_>>(), [anthropic]);
        billing.dismiss(anthropic);
        assert!(billing.active().next().is_none());
    }
}
