//! The ports a Service, or each Service a Group covers, is expected on, judged against the ports its tree listens on:
//! what makes a Port mismatch certain before the health timeout, and what the Port mismatch names per Service.
//!
//! A port the tree listens on is tied to a Service only when it is one of the Leases or one of the declared ports of
//! that Service. Nothing else tells which process of a Group's tree belongs to which covered Service.

use std::time::Duration;

use crate::protocol::{ServiceEnd, ServicePorts};

/// How long the Port mismatch of a Group waits, once seen, for each Service it covers to listen on a port of its own
/// before it is reported, whatever is left of the health timeout: a Service still on no port of its own then is told
/// as having not listened within it.
pub(crate) const REPORT_GRACE: Duration = Duration::from_secs(10);

/// The ports of one Service with a Lease to probe, as the Daemon granted and as the configuration declares them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortPlan {
    pub(crate) service: String,
    /// The Lease its health check probes.
    pub(crate) leased: u16,
    /// The port the Port name of its health check declares, the one of the Main worktree.
    pub(crate) health_declared: u16,
    /// Every Lease of the Service, `leased` included.
    pub(crate) leases: Vec<u16>,
    /// The port each of its Port names declares.
    pub(crate) declared: Vec<u16>,
}

impl PortPlan {
    /// The ports of `listening` tied to this Service, sorted as `listening` is.
    fn seen(&self, listening: &[u16]) -> Vec<u16> {
        listening
            .iter()
            .copied()
            .filter(|port| self.leases.contains(port) || self.declared.contains(port))
            .collect()
    }
}

/// What the tree of a Service is judged on while it starts: the plan of the Service, or of each Service its Group
/// covers, and every Lease of the Worktree, which a tree may listen on rightfully whatever it declares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Expectation {
    pub(crate) plans: Vec<PortPlan>,
    pub(crate) worktree_leases: Vec<u16>,
}

impl Expectation {
    /// The port of `plan` that decides a Port mismatch early: the declared port of its health check, unless it is a
    /// Lease of the Service or of any other Service of the Worktree.
    fn watched(&self, plan: &PortPlan) -> Option<u16> {
        let port = plan.health_declared;
        (!plan.leases.contains(&port) && !self.worktree_leases.contains(&port)).then_some(port)
    }

    /// Whether the tree may be judged before the health timeout. Where each health Lease is its declared port, as in
    /// the Main worktree unless that port was held when the Lease was granted, nothing is judged early.
    pub(crate) fn judged_early(&self) -> bool {
        self.plans.iter().any(|plan| self.watched(plan).is_some())
    }

    /// The Lease and the port of the first Service whose tree listens on its watched port and not on its health
    /// Lease, which makes the Port mismatch certain. A port no Service declares for its health check, such as a
    /// debugger or a fixed HMR port opened before the Lease, never decides it.
    pub(crate) fn decided(&self, listening: &[u16]) -> Option<(u16, u16)> {
        self.plans.iter().find_map(|plan| {
            let port = self.watched(plan).filter(|port| listening.contains(port))?;
            (!listening.contains(&plan.leased)).then_some((plan.leased, port))
        })
    }

    /// Whether every Service of the plans listens on a port tied to it, so that a report waits for nothing more.
    pub(crate) fn all_seen(&self, listening: &[u16]) -> bool {
        self.plans.iter().all(|plan| !plan.seen(listening).is_empty())
    }

    /// The Port mismatch of a tree listening on `listening`, named per Service of the plans. `leased` and `actual`
    /// are the pair judged before, kept unless a Service is found on its watched port instead of its Lease, whose pair
    /// then stands for the whole tree, so that one Service's port is never paired with another's Lease.
    pub(crate) fn port_mismatch(&self, listening: &[u16], leased: u16, actual: u16) -> ServiceEnd {
        let (leased, actual) = self.decided(listening).unwrap_or((leased, actual));
        let services = self
            .plans
            .iter()
            .map(|plan| ServicePorts {
                service: plan.service.clone(),
                leased: plan.leased,
                seen: plan.seen(listening),
            })
            .collect();
        let unattributed = listening
            .iter()
            .copied()
            .filter(|port| {
                !self
                    .plans
                    .iter()
                    .any(|plan| plan.leases.contains(port) || plan.declared.contains(port))
            })
            .collect();
        ServiceEnd::PortMismatch {
            leased,
            actual,
            services,
            unattributed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(service: &str, leased: u16, declared: u16) -> PortPlan {
        PortPlan {
            service: service.to_owned(),
            leased,
            health_declared: declared,
            leases: vec![leased],
            declared: vec![declared],
        }
    }

    fn of(plans: Vec<PortPlan>) -> Expectation {
        let worktree_leases = plans.iter().flat_map(|plan| plan.leases.clone()).collect();
        Expectation { plans, worktree_leases }
    }

    fn group() -> Expectation {
        of(vec![plan("api", 24004, 3000), plan("web", 24005, 3001)])
    }

    #[test]
    fn a_plan_whose_health_lease_is_its_declared_port_is_never_judged_early() {
        assert!(!of(vec![plan("web", 3000, 3000)]).judged_early());
        assert!(of(vec![plan("web", 3000, 3000), plan("api", 24004, 3001)]).judged_early());
    }

    #[test]
    fn a_health_port_declared_as_a_lease_of_another_service_of_the_worktree_is_never_watched() {
        let mut expectation = group();
        expectation.worktree_leases.push(3001);

        assert_eq!(
            expectation.decided(&[3001]),
            None,
            "3001 is the Lease of a Service of the Worktree"
        );
        assert_eq!(expectation.decided(&[3000]), Some((24004, 3000)));
    }

    #[test]
    fn a_health_port_declared_and_listened_on_instead_of_its_lease_decides() {
        assert_eq!(group().decided(&[3001]), Some((24005, 3001)));
        assert_eq!(
            group().decided(&[3000, 24005]),
            Some((24004, 3000)),
            "api is on its declared port, web on its Lease"
        );
    }

    #[test]
    fn a_port_no_health_check_declares_or_a_declared_port_next_to_its_lease_does_not_decide() {
        assert_eq!(group().decided(&[]), None);
        assert_eq!(group().decided(&[9229]), None, "a debugger opened before the Lease");
        assert_eq!(group().decided(&[24004, 9229]), None);
        assert_eq!(group().decided(&[3000, 24004]), None, "api listens on its Lease too");

        let mut hmr = plan("web", 24005, 3001);
        hmr.declared.push(3002);
        hmr.leases.push(24006);
        assert_eq!(
            of(vec![hmr]).decided(&[3002]),
            None,
            "a fixed HMR port is not the health port"
        );
    }

    #[test]
    fn every_service_seen_is_complete_and_one_missing_is_not() {
        assert!(group().all_seen(&[3000, 24005]));
        assert!(!group().all_seen(&[3000, 9229]));
    }

    #[test]
    fn each_service_is_named_with_its_own_lease_and_the_ports_tied_to_it() {
        let end = group().port_mismatch(&[3000, 3001, 9229], 24004, 3001);

        assert_eq!(
            end,
            ServiceEnd::PortMismatch {
                leased: 24004,
                actual: 3000,
                services: vec![
                    ServicePorts {
                        service: "api".to_owned(),
                        leased: 24004,
                        seen: vec![3000],
                    },
                    ServicePorts {
                        service: "web".to_owned(),
                        leased: 24005,
                        seen: vec![3001],
                    },
                ],
                unattributed: vec![9229],
            }
        );
    }

    #[test]
    fn the_pair_is_the_one_of_the_service_found_on_its_declared_port() {
        let ServiceEnd::PortMismatch { leased, actual, .. } = group().port_mismatch(&[24004, 3001], 24005, 24004)
        else {
            unreachable!("a Port mismatch is built")
        };
        assert_eq!((leased, actual), (24005, 3001));
    }

    #[test]
    fn without_a_service_on_its_declared_port_the_pair_judged_before_stays() {
        let ServiceEnd::PortMismatch {
            leased,
            actual,
            unattributed,
            ..
        } = group().port_mismatch(&[5173], 24004, 5173)
        else {
            unreachable!("a Port mismatch is built")
        };
        assert_eq!((leased, actual, unattributed), (24004, 5173, vec![5173]));
    }
}
