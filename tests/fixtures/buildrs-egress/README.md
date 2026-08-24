# `buildrs-egress`

**Property asserted:** a `build.rs` attempting a network connection from a
`none`-profile task fails, and the attempt surfaces as `EgressBlocked` rather
than as an ordinary build error.

For a task whose profile is `none` this is a finding, not a routine failure: it
is either a misconfiguration or a hostile dependency probing for a way out, and
it must be visible in mission review either way.
