# Triage Labels

The skills speak in terms of five canonical triage roles. This file maps those roles to the actual label strings used in this repo's issue tracker.

| Label in mattpocock/skills | Label in our tracker | Meaning                                  |
| -------------------------- | -------------------- | ---------------------------------------- |
| `needs-triage`             | `needs-triage`       | Maintainer needs to evaluate this issue  |
| `needs-info`               | `needs-info`         | Waiting on reporter for more information |
| `ready-for-agent`          | `ready-to-build`     | Fully specified, buildable unattended    |
| `ready-for-human`          | `needs-judgment`     | Requires a call only a person can make   |
| `wontfix`                  | `wontfix`            | Will not be actioned                     |

When a skill mentions a role (e.g. "apply the AFK-ready triage label"), use the corresponding label string from this table.

Edit the right-hand column to match whatever vocabulary you actually use.

The right-hand column deliberately avoids naming who or what does the work. These labels are visible to anyone with access to the tracker, clients included, and a repository's label vocabulary is not the place to disclose the delivery tooling. `ready-to-build` states the condition of the ticket (fully specified, no open questions), which is the useful fact either way. `needs-judgment` states why it cannot be picked up cold.
