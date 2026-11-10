# Your ChatGPT sign-in is never on disk in the clear: it is sealed with a key in the computer's keychain, or kept for the session only

- **Issue:** #19, AS-KEY
- **Upstream doc it would have touched:** `ROADMAP.md` (progress log)
- **Not verified:** the real Windows Credential Manager and the real Secret Service on Linux and FreeBSD (nothing was built or run for those targets here); the real macOS Keychain in a running app, including the prompt a rebuilt, unsigned binary gets for the key it made; a keychain that is locked and asks to be unlocked. Every test uses a keychain in memory.

The tokens were in `chatgpt-credentials.json` in the settings folder, readable by anything that could read the folder (mode 0600 on Unix only; nothing on Windows). Now `crates/assist/src/chatgpt/vault.rs` keeps them, as the owner decided:

- **With a keychain** (macOS Keychain, Windows Credential Manager, the Secret Service over D-Bus): a random 256-bit key (`ring::rand::SystemRandom`) is the only thing in the keychain, and the credentials are sealed with AES-256-GCM (`ring::aead`) in `chatgpt-credentials.sealed`, written atomically with mode 0600 like before. Windows keeps at most 2,560 bytes per credential and the tokens can be more, which is why only the key goes there.
- **Without one** (none, or it fails): memory only, for the life of the process. Nothing is written to disk, and the pane says the person signs in again next time.
- **Migration:** a `chatgpt-credentials.json` from before is sealed (or taken into memory) and deleted the first time the folder is opened. One that can't be read is deleted too, and one that can't be sealed is kept in memory.
- **Sign-out** deletes the sealed file, any plaintext file, the key and the memory copy.
- **The web build** has no ChatGPT backend, and the whole `chatgpt` module and both new dependencies are desktop only (`cfg(not(target_arch = "wasm32"))`).

The sealed file is a small JSON envelope: `format` (`wordcraft.chatgpt-credentials`), `version` (1), `algorithm` (`AES-256-GCM`), `nonce` (12 random bytes, base64url) and `ciphertext` (the credentials' JSON and the 16-byte tag, base64url, with the `base64` crate already in the build). The additional data is `wordcraft.chatgpt-credentials/1/AES-256-GCM`, so nothing else sealed with the key, now or in a later format, opens as these credentials. The envelope denies unknown fields. A file that is cut short, changed, too big, of another format, version or algorithm, or doesn't open with the key reads as signed out and stays where it is until the next sign-in replaces it, as a damaged plaintext file always did.

The key is under the keychain service `WordCraft` and the account `chatgpt-credentials-key:` followed by the first 8 bytes, in hex, of the SHA-256 of the settings folder's canonical path. So each profile, and each test's folder, has its own key.

## The window never waits for the keychain

A keychain can wait for the person (an unlock prompt, or macOS asking whether a rebuilt program may read its key) or never answer. The window's thread reads who is signed in every ten seconds, and through the offers' readiness, so `chatgpt::account` (and the sign-in's `id_token_hint`) now come from `peek`, which never asks the keychain. When the keychain has to be asked, because the folder hasn't been opened yet, an older file waits to be sealed, or the key it has doesn't open the file, `peek` answers "nobody" and starts one thread named `chatgpt keychain` to find out. `chatgpt::opening` says it is running, and while it is the window reads again every frame instead of ten seconds later. The threads that sign in, refresh and sign out ask the keychain themselves.

Asking the keychain takes no lock, so a hung keychain holds up no refresh or sign-out waiting for the credentials' lock. Making a key and moving an older file are done under that lock, so two WordCraft processes with one folder can't make two keys. A thread that already holds the lock (a refresh, a sign-in being kept) does this under its own: `Lock` now records its holder in a thread-local, because taking it again would wait for ever. A key that doesn't open the file is asked for again once, since another WordCraft may have signed out and back in with a new key. A file found damaged with the keychain's current key is remembered by its hash, so the window doesn't send a thread to the keychain about it every ten seconds.

## The dependency: keyring 4

`keyring` 4.2.0 (MIT OR Apache-2.0) with its default `v1` feature, a target-specific dependency of `wordcraft-assist` only. Its `v1` stores are the pure-Rust ones the owner asked for: `apple-native-keyring-store` (Keychain Services through `security-framework` 3.7, already in the lock), `windows-native-keyring-store` (`windows-sys` 0.61, already in the lock), and `zbus-secret-service-keyring-store` with `crypto-rust` (the Secret Service over `zbus` 5 with `async-io`, the runtime AccessKit already builds, and RustCrypto `aes`, `cbc` and `hkdf` for the service's session encryption). Nothing links libdbus or OpenSSL. `keyring` 3.6 would have brought a second `zbus` (4) on Linux.

New in the lock: `keyring`, `keyring-core`, the three stores above, `secret-service` 5.2, `cbc`, `block-padding`, `hkdf`, the `num` crates and `byteorder`: all MIT OR Apache-2.0 except `byteorder` (Unlicense OR MIT), taken under MIT as `memchr` and `aho-corasick` already are here. `ring` 0.17.14 (Apache-2.0 AND ISC, both on the allowlist) was already in the lock through rustls.

## Decisions the owner's list didn't make

- **Opt-in.** Only the desktop app calls `chatgpt::use_system_keychain()`, at start. Every other process, including every test and the CLI's gate, has no keychain (memory only) unless a test hands a folder an in-memory one (`chatgpt::fake_keychain`, `#[doc(hidden)]`). So no test can reach the person's keychain, even one that forgets to ask for the fake.
- **`Account` says where it is kept** (`kept: Kept::Keychain | Kept::Session`, serialized as `kept`). The Account page shows it under who is signed in, and the pane under its picker while ChatGPT is chosen.
- **A keychain error after a working start** (a write that can't keep the key, or a later read that fails) switches that folder to memory only for the rest of the process, as decision 3 reads. A sign-in or refresh kept in memory then deletes the sealed file on disk, which holds the tokens it replaces: left there, it would come back the next time the keychain answers, with a refresh token ChatGPT may have rotated since. A read that fails leaves the file alone, since nothing replaces it.
- **`CREDENTIALS_FILE` now names the sealed file**; the old name is `PLAINTEXT_FILE`.
