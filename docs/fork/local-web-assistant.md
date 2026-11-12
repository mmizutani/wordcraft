# ChatGPT plan usage in the local web editor

The [#116](https://github.com/mmizutani/wordcraft/issues/116) development build lets an eligible tester connect ChatGPT and rewrite selected text in a WordCraft document. Run the browser editor and its Rust Assistant API on the same loopback origin.

## Run

From the repository root, with the Rust WASM target and Trunk installed:

```sh
(cd apps/wordcraft-web && trunk build --release)
cargo run -p wordcraft-cli -- local-assistant dist/web
```

Open the full loopback URL printed by the launcher. Its fragment authorizes that browser tab to use the local Assistant. Keep the full URL private. WordCraft removes the fragment after opening and remembers the authorization across reloads in that tab. Open the Assistant pane, choose **Continue with ChatGPT**, and click **Open ChatGPT sign-in** to grant plan usage. Return to WordCraft and select up to 8 KiB of text in one paragraph. Use **Select current paragraph** if the caret is inside an eligible paragraph. The editor’s selection menu also offers **Open Assistant for Rewrite…**. If a selection contains tracked changes, resolve them in Review or select different text. Selections with hidden text are ineligible. Choose **Rewrite selected text**, then use the normal Review controls to accept or reject the ChatGPT-attributed tracked changes. Use synthetic text for pilot verification.

The launcher sets the editor's local Assistant marker while serving the Trunk build. The API binds to loopback and checks the exact HTTP authority and, for actions, the exact browser origin. OAuth uses a separate loopback callback. The local web profile keeps credentials in process memory and saves the opaque registration identifiers to support future sign-ins. Stopping the process ends the local session. After a Rewrite finishes, use **Disconnect** to clear the local grant and attempt provider revocation.

Rewrite sends the selected text to OpenAI after you click the action. The local server makes the request with your plan grant. OpenAI reports usage in [ChatGPT Settings → Usage](https://chatgpt.com/settings/usage) where applicable. Whether this uses the same allowance as Codex has yet to be confirmed. [Plan usage guide](https://developers.openai.com/siwc/token-sharing-open-source) · [Inference contract](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference)

The remotely hosted Assistant follows the separate approval and acceptance criteria in [#116](https://github.com/mmizutani/wordcraft/issues/116).

## Live verification

On 2026-11-12, a tester consented to plan usage in Chrome against the localhost build. A real Responses request rewrote a synthetic Unicode sentence. The browser staged one insertion and one deletion, both authored by ChatGPT. A separate scripted browser test accepted both changes through Review. The live check established the local plan-grant flow and editor handoff. Codex allowance attribution was outside that check.
