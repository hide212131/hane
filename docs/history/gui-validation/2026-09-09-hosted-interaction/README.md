# Hosted OS interaction evidence

[Actions run 34259421224](https://github.com/hide212131/hane/actions/runs/34259421224) passed on macOS 15.

- Hane: `866cfbc0cdcca8021aff0439b53e7c247fc7e54c`
- Procedure: `4cea7bde635203fa2bf85823ffc940e88aceea71`, `hosted-gui-interaction/2`
- ASCII keyboard input/save, separate edit undo/redo, reopen and visible-text OCR: pass.
- Japanese IME romaji conversion to `日本語`, commit/save and original input-source restoration: pass.
- OS wheel: visible LINE 1–11 moved to LINE 13–23; document bytes unchanged.
- Exact PID cleanup: pass. Evidence also passed the production receipt validator locally using this experiment's identity and timestamps.

These are focused smoke scenarios, not exhaustive GUI coverage. This experiment has not published a production GUI gate or completed Issue #44. Later heads require their own evidence.
