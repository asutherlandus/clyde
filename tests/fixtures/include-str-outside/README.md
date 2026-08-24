# `include-str-outside`

**Property asserted:** an `include_str!` reaching outside the crate directory is
a read that static analysis of the manifest cannot see. With a baseline built
from the static closure alone, the build fails; the failure is rendered as a
named path outside the confirmed baseline rather than as a raw cargo error, and
the remedy is a pin a human confirms.

This is the case learn mode exists for, and the reason the static-proposal path
is the normal one: a learn run is a wide-scope execution of exactly the code
being constrained.
