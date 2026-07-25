# Third-Party Notices

Codex Web Terminal's original source code is licensed under the
[MIT License](LICENSE). Third-party components are not relicensed under the
project's MIT License and remain subject to their respective licenses.

The exact resolved dependency versions and integrity data are recorded in:

- `server/Cargo.lock`
- `web/package-lock.json`

This source repository does not vendor dependency source trees, compiled
dependency binaries, `node_modules`, Cargo registry contents, or generated
browser bundles. Dependencies are obtained from crates.io and the npm registry
when a user builds the project.

## Browser runtime components

The generated browser bundle contains these runtime packages:

| Package | Resolved version | License |
| --- | --- | --- |
| `@xterm/addon-fit` | 0.11.0 | MIT |
| `@xterm/addon-web-links` | 0.12.0 | MIT |
| `@xterm/xterm` | 6.0.0 | MIT |
| `react` | 19.2.8 | MIT |
| `react-dom` | 19.2.8 | MIT |
| `scheduler` | 0.27.0 | MIT |

Build and test dependencies are recorded separately in
`web/package-lock.json`; they are not part of the generated runtime JavaScript
bundle.

## Rust components

The compiled server links Rust dependencies recorded in `server/Cargo.lock`.
An audit of the locked graph found license choices under MIT, Apache-2.0,
BSD-2-Clause, BSD-3-Clause, Unicode-3.0, BSL-1.0, CC0-1.0, MIT-0, the
Unlicense, and Apache-2.0 with the LLVM exception. Every locked crate declares
license metadata; no GPL-only, AGPL, SSPL, proprietary, non-commercial, or
unknown dependency was found.

Some dependencies carry additional attribution material that must remain with
redistributed binaries, including:

- `atomic-waker`'s `LICENSE-THIRD-PARTY`;
- `matchit`'s `LICENSE.httprouter`;
- the Unicode and IBM notices shipped with ICU4X packages;
- `unicode-ident`'s `LICENSE-UNICODE`;
- upstream `COPYRIGHT` and `NOTICE` files where supplied.

The lockfile is the authoritative package-and-version inventory. Registry
package pages link to each exact crate's source and license files:

- <https://crates.io/>
- <https://www.npmjs.com/>

## Binary redistribution

The repository build scripts create local Windows or Linux application
directories. This file is an inventory and warning; it is not a complete
binary-redistribution license bundle.

Before redistributing a compiled executable or generated browser assets, the
redistributor must include the complete copyright, license, attribution, and
NOTICE texts for the exact dependency versions used in that build. In
particular, preserve all upstream MIT notices, include the full Apache License
2.0 text when selecting Apache-2.0, and retain the exact Unicode and BSD
notices identified above.

No prebuilt binary release is published by this repository. A future automated
binary release process must generate and verify a complete
`THIRD_PARTY_LICENSES.txt` (or equivalent license directory) before publishing
an archive.
