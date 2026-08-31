# Benchmarks — Methodology (§4.3)

- Hardware: TBD per run
- OS: TBD
- PG version: 15/16 via testcontainers
- Data shape: fixtures in tests/fixtures (§37)
- Query: SELECT * FROM ...
- Network: localhost
- Build mode: release
- Caches: cold vs warm

Never compare pgNative against another client under different conditions as scientific evidence.
