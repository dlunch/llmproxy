# Architecture review decisions

- Accepted: retain all quota windows with independent observation/reset timestamps. A single limiting window can hide a second exhausted window after the first resets.
- Accepted: Responses length termination produces an incomplete response, not a completed response.
- Accepted: gateway lifecycle task owns provider setup as well as stream iteration; cancellation and deadline cover authentication, connection establishment and subprocess setup.
