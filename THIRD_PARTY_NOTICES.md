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

Build tools are recorded in `web/package-lock.json`. Bundlers can contribute
small generated helpers even though their full packages are not copied into
`web/dist`, so release license generation conservatively inventories every
installed locked npm package and labels it as `runtime` or `build`. A runtime
package must ship its own license/NOTICE text. A build-only fallback without a
shipped notice is rejected unless its exact package/version attribution source
has been explicitly reviewed by the generator.

## Rust components

The compiled server links Rust dependencies recorded in `server/Cargo.lock`.
An audit of the locked graph found license choices under MIT, Apache-2.0,
BSD-2-Clause, BSD-3-Clause, Unicode-3.0, BSL-1.0, CC0-1.0, MIT-0, the
Unlicense, and Apache-2.0 with the LLVM exception. Every locked crate declares
license metadata; no GPL-only, AGPL, SSPL, proprietary, non-commercial, or
unknown dependency was found.

The compiled executable also links the Rust standard library. Release bundles
therefore include the exact
`share/doc/rust/COPYRIGHT-library.html` supplied by the recorded Rust
toolchain, including the standard library's third-party notices.

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

The normal repository build scripts create local Windows or Linux application
directories. This Markdown file remains an inventory and explanation; by
itself it is not the complete binary-redistribution license bundle.

`scripts/generate-third-party-licenses.py` builds a target-specific
`THIRD_PARTY_LICENSES` directory from the exact locked non-development Cargo
graph, the Rust standard library notice, and every installed locked npm
runtime/build package. It fails on an unreviewed or malformed license
expression, missing required notice evidence, npm name/version/path mismatch,
or missing registry URL/SHA-512 integrity metadata. Its manifest records the
Rust release and host, canonical UTF-8/LF lockfile SHA-256 digests, npm role
and locked provenance, and every included normalized evidence-body digest.

Tagged GitHub Release archives are publishable only after the workflow
generates and validates:

```text
THIRD_PARTY_LICENSES/
├── THIRD_PARTY_LICENSES.txt
└── manifest.json
```

The text file contains the exact installed license, copyright, attribution,
and NOTICE evidence selected for that archive's target. The one reviewed
legacy build-only package that ships only a license declaration contributes
its exact installed `package.json`, and the manifest marks that fallback
explicitly. The generated directory, this notice, and the project's `LICENSE`
all travel with the executable and browser assets. Release automation also
rejects missing or extra package files and never substitutes a bundle
generated for another target.

Anyone redistributing a local `dist` or `dist-linux` directory must first run
the matching generator command in [BUILDING.md](BUILDING.md), review the
result, and keep the generated directory intact. A Windows GNU fallback build
has a different dependency graph from the supported public MSVC artifact and
is not covered by an MSVC bundle.
