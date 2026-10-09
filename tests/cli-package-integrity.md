# CLI connection-test package verification

The interim `cli_connection_test` artifact is separate from the desktop GUI
package. Its normal debug build compiles the production bridge SHA256 into the
launcher. Assembly checks, native hosting, guardian ownership, existing-library
resolution and account isolation use the same production launcher core. There
is no alternate connection implementation or official fallback.

`--inspect-package-bindings` exits before launcher preference, known-folder,
storage resolution or process APIs. It checks only the running executable's
adjacent bridge and emits the fixed production bindings as JSON. It does not
load the bridge DLL or invoke an installed game. Missing, changed, or differently
hashed bridge bytes are refused even if adjacent artifact metadata is replaced.

`cli-package-integrity.ps1` takes a freshly assembled production CLI package.
It performs 30 checks using copies in a uniquely owned workspace fixture:

- Directory and ZIP payload equality, including notices and all five licenses.
- The actual compiled diagnostic, with native GUI and login still unverified.
- Rejection by the desktop GUI package verifier.
- Constructor refusal of outside-workspace inputs and linked source ancestors
  before staging or diagnostic execution; the fixed Cargo target is guarded.
- Actual diagnostic rejection of missing, modified and wrong bridge bytes,
  with byte inventories unchanged by each diagnostic.
- Rejection of policy changes and native-login verification overclaims.
- Rejection of mismatched directories and duplicate or traversing ZIP entries.

Only the real CLI package diagnostic executes. Altered bridge fixtures are
never loaded, installed or sent to a game; no player client, configuration,
Realm database, library, account, hosts or certificate is accessed. Successful
fixtures are removed only after exact ownership and path checks; failures are
retained. Existing GUI package fixtures remain a separate suite.

These checks prove package assembly and compiled binding refusal, alongside
the core's synthetic runtime tests. Native login, UI, gameplay, score upload
and cleanup after a real client exits require the player's manual test.
