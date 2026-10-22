# Engineering principles

- Minimize request latency as a primary design constraint. Avoid unnecessary copying, allocation, serialization, and sequential I/O on request and streaming paths.
- Measure performance changes with representative workloads and report the scope of the measurements. Preserve correctness, validation, credential persistence, and cancellation behavior while optimizing.
