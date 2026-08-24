# `buildrs-credential-hunt`

**Property asserted:** none of the paths a credential-hunting build script would
look in are reachable from a build sandbox: no host home, no `~/.ssh`, no
`~/.gnupg`, no cloud configuration, no browser profile, and no container runtime
socket.

The script reports what it found; the test asserts it found nothing. It is
written as a survey rather than a single probe because the property is "none of
these", and a test that checked one path would pass while the others leaked.
