# Hosted WordCraft on ChatGPT Sites

## Phase 1 scope

[Issue #113](https://github.com/mmizutani/wordcraft/issues/113) defines acceptance criteria S1–S4. This phase verifies that the existing editor, visitor identity, and durable storage bindings work together on a private Site. [The parent issue](https://github.com/mmizutani/wordcraft/issues/112) tracks the later cloud-document, collaboration, assistant, and sandbox phases.

## Build and ownership

The static edition builds through Trunk into `dist/web` using [`apps/wordcraft-web/Trunk.toml`](../../apps/wordcraft-web/Trunk.toml). [`packaging/web/package.sh`](../../packaging/web/package.sh) packages that output, and [the Pages workflow](../../.github/workflows/pages.yml) supplies the pinned font and language inputs. The hosted edition reuses this build and keeps its relative asset paths. Sites packaging is a separate target, so the release ZIP and Pages workflow keep their existing consumers.

| Component | Responsibility |
| --- | --- |
| Rust/WASM editor | Load and edit the document in the browser. The existing local file workflow remains available in the pilot. |
| Sites dispatcher | Control Site access and supply authenticated visitor identity to the Worker. The Phase 1 verification version is owner-only. |
| Rust Worker | Serve the editor and gate the temporary storage probe. Consume the platform-authenticated Site-scoped user ID on the server. |
| D1 and R2 | Hold a synthetic probe record and its object bytes. Later phases use these roles for document metadata and document contents. |

## Verify the deployment boundary first

Package a minimal Rust Worker with generated ESM at `dist/server/index.js`, its generated module files, and a tiny browser WASM asset under `dist/client`. Declare the managed D1 and R2 bindings in `.openai/hosting.json` with the real Site project ID and the server-backed deployment profile. Include a migration for the probe table. Build the archive from the exact source revision pushed to the Site repository and record the saved deployment version.

Use a dedicated checkout with the Rust `wasm32-unknown-unknown` target, `worker-build`, and Trunk installed. Run `packaging/sites/build.sh --probe` for the synthetic verification bundle. With the Pages font and repository-pinned language inputs set through `CRAFT_FONTS_DIR` and `CRAFT_I18N_DIR`, run `packaging/sites/build.sh --editor` for the full editor. Both commands stage the Worker and browser assets under `dist/`. The editor build omits the temporary storage route. Use the Sites source and archive workflow to publish the staged output to the same owner-private Site.

Cloudflare documents [Rust Workers](https://developers.cloudflare.com/workers/languages/rust/), and Sites documents [managed identity and storage](https://learn.chatgpt.com/docs/sites/data-and-connections). The [capability probe results](#capability-probe-results) record live verification of the generated server WASM modules and browser asset routing.

Package the full Trunk build with the same font inputs as Pages. Sites rejected the 62,364,711-byte raw editor WASM from `2a70a95a89643fa0b3d2d71c8eee2d0bdb7d4774` at the same [25 MiB per-file limit](https://developers.cloudflare.com/workers/platform/limits/#static-assets) as Cloudflare Workers. The package compresses an oversized Trunk WASM file with deterministic gzip. The Worker streams it through the original content-hashed URL with `application/wasm`, `Content-Encoding: gzip`, and manual response encoding. The browser decodes the same WASM bytes, preserving the bundled fonts.

The Pages build measured 62,432,793 bytes of uncompressed editor WASM on 2026-11-10. If a future gzip asset exceeds the limit, evaluate serving the unchanged WASM from an immutable, versioned R2 object at the same-origin asset path with `application/wasm`. Verify that only the owner can seed the object, and check its digest and availability before deploying an editor that references it.

## Storage and identity probe

The owner opens the private Site and runs separate write, read, and delete requests against the same fresh probe ID. The Worker derives ownership from the trusted Sites identity and accepts a fixed synthetic payload. D1 holds the probe ID, its owner, and the R2 object reference. R2 holds the fixed payload. The read confirms the matching record and exact bytes. The delete removes both, and a follow-up read confirms absence.

Gate the probe at the Worker as well as at the Site. Requests without an authenticated visitor receive an authorization failure before any storage call. Probe responses carry no identity headers, tokens, or resource credentials. If a storage step fails, report failure, clean up any partial writes, and verify cleanup before calling the run successful. The probe build trusts identity provided by Sites dispatch and must stay behind that boundary. It establishes binding access and failure visibility. The cloud-document phase will define the actual commit and recovery protocol across the two stores.

## Verification

| Claim | Evidence |
| --- | --- |
| S1. Private editor and font coverage | Open the deployed Site in an authenticated browser. Confirm the release WASM loads and the editor accepts an edit. Compare a representative multilingual document with the same-source static build and record the font inputs, asset byte sizes, delivery route, and deployment revision. |
| S2. Identity and managed storage | Verify live write, read, and delete across separate requests. Check that a signed-out session is denied and exercise the Worker's missing-identity guard without a storage write. |
| S3. Repeatable deployment and portability | Record the tested build and packaging commands, source revision, resource binding names, and live results. Keep configuration examples free of credentials. Mark each untested capability and the phase that will verify it. |
| S4. Static compatibility | Build the existing web package and open it under a URL subpath. Verify the editor loads and the local open and save workflow still works. |

### Capability probe results

The first private version, built from `9c06e307d1a087b072669df397fe70580a1a53df`, ran the Rust-generated Worker and served the eight-byte browser WASM module as `application/wasm`. Managed D1 applied the probe migration. Signed-out requests were denied at the Site. The Worker returned 401 for a Site service request without visitor identity. In the signed-in owner's browser, the four storage requests returned `201`, `200`, `204`, and `404`. The Worker compares the exact R2 bytes before returning 200.

The same-source static package loaded under `/editor/` in headless Chrome. Opening a multilingual text fixture and using the browser download fallback produced a DOCX that retained its text. The private capability probe and full editor use separate source revisions because Sites reused the saved version when given a different archive for the same commit.

The local gzip package passed Wrangler's dry run and initialized the editor in Chrome. Its 22,736,873-byte response decoded to the original 62,364,711-byte WASM, with a matching SHA-256 digest.

Sites published the full editor from `cdb382ed0ebfe76f7cbcc753072d78feb2ceb1dc`. The hosted 22,736,872-byte gzip asset decoded to 62,364,711 bytes with the same SHA-256 as that release build (`589d760682d25a877b1e90b6ff3cb95d01a18d1f90d75078e7ba9e59c13cad6f`). The live Site denied signed-out traffic, and the full editor version returned 404 for the temporary probe route.

In signed-in Chrome, the hosted editor loaded that asset, opened the same multilingual fixture as the static build, accepted an edit, and saved through the browser download fallback. The downloaded DOCX contained both original lines and the new edit. Both builds showed the same missing Japanese glyphs with the fixture's default font.

## Boundaries for later phases

- **Document authority and collaboration.** The storage service owns stable document IDs, access, and committed revisions. An active room will persist accepted changes before acknowledgment, and explicit Save will publish a DOCX through that room. This separation follows the public [WOPI coauthoring precedent](https://learn.microsoft.com/en-us/microsoft-365/cloud-storage-partner-program/online/scenarios/coauth) and the repository's [network-room design](../collaboration.md). The browser-only rollout precedes desktop and offline participation. Their design will account for base revisions, pending edits, replay, and conflict handling. A separate HTTPS room service is an option if Sites cannot provide the necessary coordination.
- **Personal Cloudflare.** Reuse the editor, the Worker code where the runtime contract permits, and the logical storage bindings. A standalone deployment owns its own account resources, routing, and sign-in verification. Keep platform identity behind a server-side boundary, since Site-scoped identifiers cannot be carried across deployments as account identifiers. Data and account migration are separate work.
- **Assistant and sandbox.** The browser assistant requires approved hosted use of each eligible tester's ChatGPT plan. A curated managed sandbox may use a separate maintainer-owned OpenAI Platform project. These grants and their verification belong to their respective phases.
