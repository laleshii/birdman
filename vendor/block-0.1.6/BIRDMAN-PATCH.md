# Birdman compatibility patch

This is the MIT-licensed `block` crate version 0.1.6, vendored unchanged except
for replacing its uninhabited opaque `Class` enum with an inhabited opaque
byte, and spelling the crate's implicit C ABIs explicitly. Rust's
`uninhabited_static` future-incompatibility lint otherwise warns on
`_NSConcreteStackBlock` and is scheduled to become a hard compiler error;
Rust 1.96 also deprecates omitted ABIs.

The crate is a transitive macOS dependency of `gpui 0.2.2`. Remove this patch
when GPUI no longer depends on `block 0.1.6`.
