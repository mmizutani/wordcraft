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

Cloudflare documents [Rust Workers](https://developers.cloudflare.com/workers/languages/rust/), and Sites documents [managed identity and storage](https://learn.chatgpt.com/docs/sites/data-and-connections). Live acceptance of the generated server WASM modules and routing of the browser asset under a custom Worker are open questions. Verify both before expanding the implementation. If either fails, record the failure and revisit the packaging boundary under the repository's Rust requirement.

Next package the full Trunk build with the same font inputs as Pages. The Pages WASM measured 62,432,793 bytes uncompressed on 2026-11-10. Personal Cloudflare Workers documents a [25 MiB per-file static asset limit](https://developers.cloudflare.com/workers/platform/limits/#static-assets). Measure the candidate and test the managed Sites upload and browser load directly. If the full WASM cannot ship as a managed static asset, test serving its unchanged bytes from an immutable, versioned R2 object at the corresponding same-origin asset path. That path requires a verified owner-only seeding step. Verify the object digest and availability before deploying a build that references it, and serve it with `application/wasm`. Preserve font coverage in either delivery path.

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

## Boundaries for later phases

- **Document authority and collaboration.** The storage service owns stable document IDs, access, and committed revisions. An active room will persist accepted changes before acknowledgment, and explicit Save will publish a DOCX through that room. This separation follows the public [WOPI coauthoring precedent](https://learn.microsoft.com/en-us/microsoft-365/cloud-storage-partner-program/online/scenarios/coauth) and the repository's [network-room design](../collaboration.md). The browser-only rollout precedes desktop and offline participation. Their design will account for base revisions, pending edits, replay, and conflict handling. A separate HTTPS room service is an option if Sites cannot provide the necessary coordination.
- **Personal Cloudflare.** Reuse the editor, the Worker code where the runtime contract permits, and the logical storage bindings. A standalone deployment owns its own account resources, routing, and sign-in verification. Keep platform identity behind a server-side boundary, since Site-scoped identifiers cannot be carried across deployments as account identifiers. Data and account migration are separate work.
- **Assistant and sandbox.** The browser assistant requires approved hosted use of each eligible tester's ChatGPT plan. A curated managed sandbox may use a separate maintainer-owned OpenAI Platform project. These grants and their verification belong to their respective phases.
