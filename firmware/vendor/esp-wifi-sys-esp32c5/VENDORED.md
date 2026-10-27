# Vendored esp-wifi-sys-esp32c5 0.3.0 with newer Wi-Fi libraries

`firmware/radio-c5/Cargo.toml` patches crates.io's `esp-wifi-sys-esp32c5` 0.3.0 with this
copy. esp-radio 1.0.0-beta.1 depends on `^0.3.0`, so the patch covers it as well.

The crate is the crates.io 0.3.0 package (minus `Cargo.toml.orig`, `Cargo.lock` and
`.cargo_vcs_info.json`) with seven ESP32-C5 Wi-Fi libraries in `libs/` replaced. The
crates.io originals are byte-identical to espressif/esp32-wifi-lib
[`7cd4d7b`](https://github.com/espressif/esp32-wifi-lib/commit/7cd4d7b2b4acc38aead43b029ac9409c3eba9c54)
(2026-07-24). The replacements come from
[`9e91edc`](https://github.com/espressif/esp32-wifi-lib/commit/9e91edcb2e64b5b137c78c9755296f97f414050c)
(2026-08-25), "fix(ftm): tolerate small t2/t3 inversion in measurement validation". That
commit updates `esp32c5/` too. Without it, FTM sessions between two ESP32-C3 boards
always ended with `FTM_STATUS_NO_VALID_MSMT` (measured in the private
[esp-wifi-sync](https://github.com/azais-corentin/esp-wifi-sync) experiments). We have not
yet measured whether the C5 blobs have the same problem.

| file | sha256 |
|---|---|
| libcore.a | 72e2309d83835d75568cf74efa693f88b0ef8da53fb31a0e51918b4e128de06c |
| libespnow.a | 0c509170fa1eab883f5c7e2de23ab396cb1d59bcd48fbbfb54ac17e026c2d6dc |
| libmesh.a | 42f99c86ee59c5383b276a0099bc70948d3ef2d340a1ca77dbd64af8bbec939c |
| libnet80211.a | cbee9ed8316382d838250192da730dd358b3ca5fae5b8e7f59316c7f8b506624 |
| libpp.a | d839f669fca847d072b1860bc3b4c8ff0806dedae4b866e80bc7410022c8804e |
| libsmartconfig.a | 1cc38a2fcce31952cb4db9ba4e1821dbec6c6f379534cc5e3e97a7508bf53042 |
| libwapi.a | 97391b40306d28cb054bf3613f73420c2e067283e062526a617a9da161636de7 |

Refresh with:

```sh
for f in libcore.a libespnow.a libmesh.a libnet80211.a libpp.a libsmartconfig.a libwapi.a; do
  curl -fL -o libs/$f https://raw.githubusercontent.com/espressif/esp32-wifi-lib/<commit>/esp32c5/$f
done
```

Drop this patch once an esp-radio release ships `esp-wifi-sys-esp32c5` with libraries at or
after `9e91edc`. esp-radio 1.0.0-beta.1 also leaves the FTM bits out of
`g_wifi_feature_caps` and hard-codes `ftm_responder = false` for the SoftAP. The firmware
has to set both itself, as esp-wifi-sync's `firmware/src/bin/ftm_sync.rs` does.
