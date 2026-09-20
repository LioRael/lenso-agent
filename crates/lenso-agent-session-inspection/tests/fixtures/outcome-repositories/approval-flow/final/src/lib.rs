#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishResult {
    Applied,
    RejectedStale,
}

#[derive(Debug)]
pub struct Proposal {
    revision: u64,
    published: bool,
}

pub fn create_proposal(revision: u64) -> Proposal {
    Proposal {
        revision,
        published: false,
    }
}

pub fn publish(proposal: &mut Proposal, approval_revision: u64) -> PublishResult {
    if approval_revision == proposal.revision {
        proposal.published = true;
        PublishResult::Applied
    } else {
        PublishResult::RejectedStale
    }
}

pub fn was_published(proposal: &Proposal) -> bool {
    proposal.published
}

#[cfg(test)]
mod tests {
    use super::{PublishResult, create_proposal, publish, was_published};

    #[test]
    fn applies_only_the_current_approved_revision() {
        let mut proposal = create_proposal(2);
        assert_eq!(publish(&mut proposal, 2), PublishResult::Applied);
        assert!(was_published(&proposal));
    }

    #[test]
    fn rejects_stale_approval_without_a_second_effect() {
        let mut proposal = create_proposal(2);
        assert_eq!(publish(&mut proposal, 1), PublishResult::RejectedStale);
        assert!(!was_published(&proposal));
    }
}
