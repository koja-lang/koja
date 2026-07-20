# Changelog

## Unreleased

Initial release.

- Start a fixed-size resource pool with `Pool.start`, backed by a user-supplied factory closure.
- Borrow resources with `Pool.checkout`, queueing callers in FIFO order when every resource is lent out.
- Return resources with `Pool.checkin`, preserving updates made while borrowed.
- Replace broken resources with `Pool.discard`, which rebuilds them with the factory.
