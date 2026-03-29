// Adapted from: sui/consensus/config/src/committee.rs
// Original copyright: Copyright (c) Mysten Labs, Inc.
// License: Apache 2.0 — https://www.apache.org/licenses/LICENSE-2.0
// Changes: removed fastcrypto, mysten_network, AuthorityName, NetworkPublicKey,
//          ProtocolPublicKey; Authority now only holds stake and hostname.

use std::{
    fmt::{Display, Formatter},
    ops::{Index, IndexMut},
};

/// Epoch number of a committee.
pub type Epoch = u64;

/// Voting power of one authority.
/// Total stake across all authorities should sum to 10,000 in production,
/// but any non-zero positive values are valid for testing.
pub type Stake = u64;

/// Committee of the consensus protocol for one epoch.
#[derive(Clone, Debug)]
pub struct Committee {
    epoch: Epoch,
    total_stake: Stake,
    quorum_threshold: Stake,
    validity_threshold: Stake,
    authorities: Vec<Authority>,
}

impl Committee {
    pub fn new(epoch: Epoch, authorities: Vec<Authority>) -> Self {
        assert!(!authorities.is_empty(), "Committee cannot be empty!");
        assert!(
            authorities.len() < u32::MAX as usize,
            "Too many authorities ({})!",
            authorities.len()
        );

        let total_stake: Stake = authorities.iter().map(|a| a.stake).sum();
        assert_ne!(total_stake, 0, "Total stake cannot be zero!");

        // Tolerate integer f faults when total stake is 3f+1.
        let fault_tolerance = (total_stake - 1) / 3;
        let quorum_threshold = total_stake - fault_tolerance;
        let validity_threshold = fault_tolerance + 1;
        assert!(
            2 * quorum_threshold - fault_tolerance > total_stake,
            "Quorum must intersect: quorum={quorum_threshold}, fault={fault_tolerance}, total={total_stake}"
        );

        Self {
            epoch,
            total_stake,
            quorum_threshold,
            validity_threshold,
            authorities,
        }
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    pub fn total_stake(&self) -> Stake {
        self.total_stake
    }

    pub fn quorum_threshold(&self) -> Stake {
        self.quorum_threshold
    }

    pub fn validity_threshold(&self) -> Stake {
        self.validity_threshold
    }

    pub fn stake(&self, index: AuthorityIndex) -> Stake {
        debug_assert!(
            index.value() < self.authorities.len(),
            "AuthorityIndex {} out of range (size {})",
            index.value(),
            self.authorities.len()
        );
        self.authorities[index].stake
    }

    pub fn authority(&self, index: AuthorityIndex) -> &Authority {
        debug_assert!(
            index.value() < self.authorities.len(),
            "AuthorityIndex {} out of range (size {})",
            index.value(),
            self.authorities.len()
        );
        &self.authorities[index]
    }

    pub fn authorities(&self) -> impl Iterator<Item = (AuthorityIndex, &Authority)> {
        self.authorities
            .iter()
            .enumerate()
            .map(|(i, a)| (AuthorityIndex(i as u32), a))
    }

    /// Returns true if the provided stake has reached quorum (2f+1).
    pub fn reached_quorum(&self, stake: Stake) -> bool {
        stake >= self.quorum_threshold
    }

    /// Returns true if the provided stake has reached validity (f+1).
    pub fn reached_validity(&self, stake: Stake) -> bool {
        stake >= self.validity_threshold
    }

    /// Converts a usize index to AuthorityIndex, returns None if out of range.
    pub fn to_authority_index(&self, index: usize) -> Option<AuthorityIndex> {
        if index < self.authorities.len() {
            Some(AuthorityIndex(index as u32))
        } else {
            None
        }
    }

    pub fn is_valid_index(&self, index: AuthorityIndex) -> bool {
        index.value() < self.size()
    }

    pub fn size(&self) -> usize {
        self.authorities.len()
    }
}

/// One authority in the committee. Intentionally minimal — no crypto keys.
#[derive(Clone, Debug)]
pub struct Authority {
    pub stake: Stake,
    pub hostname: String,
}

/// Unique index of an authority within one epoch's committee.
/// Values are dense: 0 .. committee.size()-1.
#[derive(Eq, PartialEq, Ord, PartialOrd, Clone, Copy, Debug, Default, Hash)]
pub struct AuthorityIndex(u32);

impl AuthorityIndex {
    pub const ZERO: Self = Self(0);
    pub const MIN: Self = Self::ZERO;
    pub const MAX: Self = Self(u32::MAX);

    pub fn value(&self) -> usize {
        self.0 as usize
    }

    pub fn new_for_test(index: u32) -> Self {
        Self(index)
    }
}

impl Display for AuthorityIndex {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}]", self.value())
    }
}

impl<T, const N: usize> Index<AuthorityIndex> for [T; N] {
    type Output = T;
    fn index(&self, index: AuthorityIndex) -> &Self::Output {
        self.get(index.value()).unwrap()
    }
}

impl<T> Index<AuthorityIndex> for Vec<T> {
    type Output = T;
    fn index(&self, index: AuthorityIndex) -> &Self::Output {
        self.get(index.value()).unwrap()
    }
}

impl<T, const N: usize> IndexMut<AuthorityIndex> for [T; N] {
    fn index_mut(&mut self, index: AuthorityIndex) -> &mut Self::Output {
        self.get_mut(index.value()).unwrap()
    }
}

impl<T> IndexMut<AuthorityIndex> for Vec<T> {
    fn index_mut(&mut self, index: AuthorityIndex) -> &mut Self::Output {
        self.get_mut(index.value()).unwrap()
    }
}

/// Create a uniform-stake test committee (1 stake per authority).
pub fn make_test_committee(epoch: Epoch, num_authorities: usize) -> Committee {
    debug_assert!(
        num_authorities > 0,
        "committee must have at least one authority"
    );
    let authorities = (0..num_authorities)
        .map(|i| Authority {
            stake: 1,
            hostname: format!("node-{i}"),
        })
        .collect();
    Committee::new(epoch, authorities)
}
