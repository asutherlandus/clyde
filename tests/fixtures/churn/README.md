# `churn`

**Property asserted:** **zero** drift prompts across an edit/check/test loop that
creates, renames, moves, and deletes files inside a granted subtree.

This is the property that decides whether the whole access-baseline control is
usable. A control that fires on ordinary editing gets clicked through, and a
control that gets clicked through is worth nothing. First-party development must
be prompt-free by construction, so grants are subtrees rather than file lists.
