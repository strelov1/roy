//! Static registry of SubscriberKind → ctor. Each kind builds a
//! `Box<dyn Subscriber>` from a JSON config string.

use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::Result;

use super::Subscriber;
use crate::types::SubscriberKind;

pub type SubscriberCtor = fn(config_json: &str) -> Result<Box<dyn Subscriber>>;

pub fn registry() -> &'static HashMap<SubscriberKind, SubscriberCtor> {
    static R: OnceLock<HashMap<SubscriberKind, SubscriberCtor>> = OnceLock::new();
    R.get_or_init(|| {
        let mut m: HashMap<SubscriberKind, SubscriberCtor> = HashMap::new();
        m.insert(SubscriberKind::Webhook, super::webhook::build);
        m.insert(SubscriberKind::NotifyNative, super::notify_native::build);
        m
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_kinds_registered() {
        for kind in [SubscriberKind::Webhook, SubscriberKind::NotifyNative] {
            assert!(
                registry().contains_key(&kind),
                "registry missing ctor for {:?}",
                kind
            );
        }
    }
}
