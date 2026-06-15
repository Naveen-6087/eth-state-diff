use std::collections::BTreeSet;
use std::str::FromStr;

use crate::diff::{AccountDiff, StateDiff};
use crate::primitives::{Address, B256, U256};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Balance,
    Nonce,
    Code,
    Storage,
}

impl FromStr for Kind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "balance" => Ok(Kind::Balance),
            "nonce" => Ok(Kind::Nonce),
            "code" => Ok(Kind::Code),
            "storage" => Ok(Kind::Storage),
            other => Err(format!("unknown change kind `{other}` (expected balance, nonce, code or storage)")),
        }
    }
}

/// Selects which accounts and fields of a diff to keep. An empty set means
/// "no restriction".
#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub include: BTreeSet<Address>,
    pub exclude: BTreeSet<Address>,
    pub kinds: BTreeSet<Kind>,
    pub slots: BTreeSet<B256>,
    /// Drop balance changes whose absolute delta is below this many wei.
    pub min_balance_delta: Option<U256>,
}

impl Filter {
    pub fn is_noop(&self) -> bool {
        self.include.is_empty()
            && self.exclude.is_empty()
            && self.kinds.is_empty()
            && self.slots.is_empty()
            && self.min_balance_delta.is_none()
    }

    fn wants(&self, kind: Kind) -> bool {
        let kind_ok = self.kinds.is_empty() || self.kinds.contains(&kind);
        // Selecting slots narrows the output to storage.
        kind_ok && (self.slots.is_empty() || kind == Kind::Storage)
    }

    fn account(&self, mut acct: AccountDiff) -> Option<AccountDiff> {
        if !self.wants(Kind::Balance) {
            acct.balance = None;
        }
        if let (Some(min), Some(delta)) = (self.min_balance_delta, acct.balance_delta()) {
            if delta.magnitude < min {
                acct.balance = None;
            }
        }
        if !self.wants(Kind::Nonce) {
            acct.nonce = None;
        }
        if !self.wants(Kind::Code) {
            acct.code = None;
            acct.code_hash = None;
        }
        if !self.wants(Kind::Storage) {
            acct.storage.clear();
        } else if !self.slots.is_empty() {
            acct.storage.retain(|slot, _| self.slots.contains(slot));
        }
        acct.has_changes().then_some(acct)
    }

    pub fn apply(&self, diff: &StateDiff) -> StateDiff {
        if self.is_noop() {
            return diff.clone();
        }
        let accounts = diff
            .accounts
            .iter()
            .filter(|(addr, _)| self.include.is_empty() || self.include.contains(addr))
            .filter(|(addr, _)| !self.exclude.contains(addr))
            .filter_map(|(addr, acct)| self.account(acct.clone()).map(|a| (*addr, a)))
            .collect();
        StateDiff { accounts }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::parse_traces_str;

    fn sample() -> StateDiff {
        let json = r#"{"pre":{
            "0x0101010101010101010101010101010101010101":{"balance":"0x100","nonce":1,"storage":{"0x1":"0x1","0x2":"0x2"}},
            "0x0202020202020202020202020202020202020202":{"balance":"0x100"}},
          "post":{
            "0x0101010101010101010101010101010101010101":{"balance":"0x90","nonce":2,"storage":{"0x1":"0x5","0x2":"0x6"}},
            "0x0202020202020202020202020202020202020202":{"balance":"0x101"}}}"#;
        StateDiff::from_prestate(&parse_traces_str(json).unwrap()[0].diff)
    }

    #[test]
    fn noop_keeps_everything() {
        assert_eq!(Filter::default().apply(&sample()), sample());
    }

    #[test]
    fn address_filters() {
        let a = Address([1; 20]);
        let only_a = Filter { include: [a].into(), ..Filter::default() }.apply(&sample());
        assert_eq!(only_a.accounts.keys().collect::<Vec<_>>(), vec![&a]);
        let not_a = Filter { exclude: [a].into(), ..Filter::default() }.apply(&sample());
        assert!(!not_a.accounts.contains_key(&a));
        assert_eq!(not_a.accounts.len(), 1);
    }

    #[test]
    fn kind_slot_and_threshold_filters() {
        let storage_only = Filter { kinds: [Kind::Storage].into(), ..Filter::default() }.apply(&sample());
        assert_eq!(storage_only.accounts.len(), 1, "account without storage changes is dropped");
        let acct = storage_only.accounts.values().next().unwrap();
        assert!(acct.balance.is_none() && acct.nonce.is_none());
        assert_eq!(acct.storage.len(), 2);

        let slot = B256::from_word("0x2").unwrap();
        let one_slot = Filter { slots: [slot].into(), ..Filter::default() }.apply(&sample());
        assert_eq!(
            one_slot.accounts.values().next().unwrap().storage.keys().collect::<Vec<_>>(),
            vec![&slot]
        );

        let big = Filter {
            min_balance_delta: Some(U256::from_u128(2)),
            kinds: [Kind::Balance].into(),
            ..Filter::default()
        }
        .apply(&sample());
        assert_eq!(big.accounts.len(), 1, "the 1 wei change is below the threshold");
    }

    #[test]
    fn kind_parsing() {
        assert_eq!("Storage".parse::<Kind>(), Ok(Kind::Storage));
        assert!("gas".parse::<Kind>().is_err());
    }
}
