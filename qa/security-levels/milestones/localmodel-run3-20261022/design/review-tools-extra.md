# Two more test-machine facts for the reviewer (integrator, from the same shakedown)

- In the Moderate shakedown (screens-v7 M00-M16) the product's `config.toml` was read-only to the tester, as in L01 and
  L02. The first start showed the trust prompt; after "1. Yes, continue", Moderate saved and `r`, the restarted
  session showed no trust prompt (M01).
- In that shakedown the tester's home had no `~/.zshrc` before the run (the lane homes start empty apart from what
  the harness creates); `~/.zshrc` was not created or read there.
