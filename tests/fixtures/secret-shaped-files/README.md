# `secret-shaped-files`

**Property asserted:** a subtree grant over `backend/auth` does **not** admit
`backend/auth/.env.local`, `backend/auth/server.pem`, or `backend/auth/id_rsa`.
Absolute exclusions are applied *before* grants and cannot be admitted by one.

A control that let a grant admit a secret-shaped file would hand every credential
in the tree to hostile dependency code the moment someone approved a mission over
the directory that held it.
