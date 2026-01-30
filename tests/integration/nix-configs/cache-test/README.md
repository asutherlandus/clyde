# Cache Test Fixture

This directory contains a flake.nix with a unique package (cowsay) to test Nix store persistence.

## Test Steps

1. First run - should download cowsay:
   ```bash
   cd tests/integration/nix-configs/cache-test
   time clyde -- --version
   ```
   Note the time (expect 30s-60s for first download)

2. Second run - should use cache:
   ```bash
   time clyde -- --version
   ```
   Should be significantly faster (<10s) because cowsay is cached

3. Verify cowsay is available inside container:
   ```bash
   clyde
   # Inside container:
   cowsay "Hello from Nix!"
   ```

4. Test garbage collection:
   ```bash
   clyde --nix-gc
   ```
   Note: This removes ALL unused packages from the store
