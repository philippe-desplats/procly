//! The ports a Service, or each Service a Group covers, is expected on, judged against the ports its tree listens on:
//! what makes a Port mismatch certain before the health timeout, and what the Port mismatch names per Service.
//!
//! A port the tree listens on is tied to a Service only when it is one of the Leases or one of the declared ports of
//! that Service. Nothing else tells which process of a Group's tree belongs to which covered Service.

use crate::protocol::{ServiceEnd, ServicePorts};

/// The ports of one Service with a Lease, as the Daemon granted and as the configuration declares them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortPlan {
    pub(crate) service: String,
    /// The Lease its health check probes.
    pub(crate) leased: u16,
    /// Every Lease of the Service, `leased` included.
    pub(crate) leases: Vec<u16>,
    /// The port each of its Port names declares, the one of the Main worktree.
    pub(crate) declared: Vec<u16>,
}

impl PortPlan {
    /// Its declared ports that are none of its Leases, which only a tree ignoring the Leases listens on.
    fn fixed(&self) -> impl Iterator<Item = u16> + '_ {
        self.declared.iter().copied().filter(|port| !self.leases.contains(port))
    }

    /// The ports of `listening` tied to this Service, sorted as `listening` is.
    fn seen(&self, listening: &[u16]) -> Vec<u16> {
        listening
            .iter()
            .copied()
            .filter(|port| self.leases.contains(port) || self.declared.contains(port))
            .collect()
    }
}

/// Whether any of the `plans` may be judged before the health timeout, a declared port of it being none of its
/// Leases. In the Main worktree a Lease is the declared port, and nothing is judged early.
pub(crate) fn judged_early(plans: &[PortPlan]) -> bool {
    plans.iter().any(|plan| plan.fixed().next().is_some())
}

/// The Port mismatch the tree listening on `listening` already is, if it is one: it listens on a declared port of a
/// Service that is not its Lease, and not on the Lease the health check of that Service probes. A port no Service
/// declares, such as a debugger opened before the Lease, never decides it.
pub(crate) fn certain(plans: &[PortPlan], listening: &[u16]) -> Option<ServiceEnd> {
    let (leased, actual) = own_pair(plans, listening)?;
    Some(report(plans, listening, leased, actual))
}

/// The Port mismatch of a tree listening on `listening`, named per Service of `plans`. `leased` and `actual` are the
/// pair judged without the plans, kept unless a Service is found on a declared port of its own instead of its Lease,
/// whose pair then stands for the whole tree, so that one Service's port is never paired with another's Lease.
pub(crate) fn port_mismatch(plans: &[PortPlan], listening: &[u16], leased: u16, actual: u16) -> ServiceEnd {
    let (leased, actual) = own_pair(plans, listening).unwrap_or((leased, actual));
    report(plans, listening, leased, actual)
}

/// The Lease of the first Service found on a declared port of its own and not on that Lease, with that port.
fn own_pair(plans: &[PortPlan], listening: &[u16]) -> Option<(u16, u16)> {
    plans.iter().find_map(|plan| {
        let on_fixed = plan.fixed().find(|port| listening.contains(port))?;
        (!listening.contains(&plan.leased)).then_some((plan.leased, on_fixed))
    })
}

fn report(plans: &[PortPlan], listening: &[u16], leased: u16, actual: u16) -> ServiceEnd {
    let services: Vec<ServicePorts> = plans
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
            !plans
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

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(service: &str, leased: u16, declared: u16) -> PortPlan {
        PortPlan {
            service: service.to_owned(),
            leased,
            leases: vec![leased],
            declared: vec![declared],
        }
    }

    fn group() -> Vec<PortPlan> {
        vec![plan("api", 24004, 3000), plan("web", 24005, 3001)]
    }

    #[test]
    fn a_plan_whose_leases_are_its_declared_ports_is_never_judged_early() {
        assert!(!judged_early(&[plan("web", 3000, 3000)]));
        assert!(judged_early(&[plan("web", 3000, 3000), plan("api", 24004, 3001)]));
    }

    #[test]
    fn a_declared_port_listened_on_instead_of_its_lease_is_certain() {
        let pair = |listening: &[u16]| match certain(&group(), listening) {
            Some(ServiceEnd::PortMismatch { leased, actual, .. }) => Some((leased, actual)),
            _ => None,
        };
        assert_eq!(pair(&[3001]), Some((24005, 3001)));
        assert_eq!(
            pair(&[3000, 24005]),
            Some((24004, 3000)),
            "api is on its declared port, web on its Lease"
        );
    }

    #[test]
    fn a_port_no_service_declares_or_a_declared_port_next_to_its_lease_is_not_certain() {
        assert_eq!(certain(&group(), &[]), None);
        assert_eq!(certain(&group(), &[9229]), None, "a debugger opened before the Lease");
        assert_eq!(certain(&group(), &[24004, 9229]), None);
        assert_eq!(certain(&group(), &[3000, 24004]), None, "api listens on its Lease too");
    }

    #[test]
    fn each_service_is_named_with_its_own_lease_and_the_ports_tied_to_it() {
        let end = port_mismatch(&group(), &[3000, 3001, 9229], 24004, 3001);

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
        let ServiceEnd::PortMismatch { leased, actual, .. } = port_mismatch(&group(), &[24004, 3001], 24005, 24004)
        else {
            unreachable!("a Port mismatch is built")
        };
        assert_eq!((leased, actual), (24005, 3001));
    }

    #[test]
    fn without_a_service_on_its_declared_port_the_pair_judged_without_the_plans_stays() {
        let ServiceEnd::PortMismatch {
            leased,
            actual,
            unattributed,
            ..
        } = port_mismatch(&group(), &[5173], 24004, 5173)
        else {
            unreachable!("a Port mismatch is built")
        };
        assert_eq!((leased, actual, unattributed), (24004, 5173, vec![5173]));
    }
}
