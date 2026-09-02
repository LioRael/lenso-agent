# Agent Plugin Selection Authority Capability

`lenso.agent.plugin-selection-authority@1` lets one Console Tool provider ask the selected Host authority to enable or disable one exact Plugin Instance. The authority owns compare-and-swap mutation of the visible Plugin Root selection; the Host still owns Generation staging and routing.

The role is deliberately separate from Plugin configuration publication. A Host may expose configuration without selection support, in which case the selection operation returns `unsupported`.
