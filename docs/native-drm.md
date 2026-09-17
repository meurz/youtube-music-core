# Native protected playback investigation

The native desktop target must play through an operating-system media pipeline
without loading an embedded or hidden browser. Version 0.8's optional WebView2
helper does not implement this target. Clear direct, DASH and SABR delivery are
already native; this investigation concerns encrypted media only.

## Windows PlayReady

Windows exposes `MediaPlayer.ProtectionManager` and
`MediaProtectionManager.ServiceRequested`. A
`PlayReadyLicenseAcquisitionServiceRequest` can generate an opaque request with
`GenerateManualEnablingChallenge` and consume an opaque response with
`ProcessManualEnablingResponse`. The operating system owns the DRM implementation
and content keys. The host and core handle transport, cancellation and playback
lifecycle, without extracting content keys.

The current official YouTube player contains a `DRM_SYSTEM_PLAYREADY` branch in
its `player/get_drm_license` flow. Its request includes the video, playback nonce,
session identifier, server-provided DRM parameters and a base64 license challenge.
The response has a license status and an opaque license. This establishes a
protocol candidate, not that every Music track offers PlayReady or that an
arbitrary desktop client will receive a usable license.

Read-only checks of three playable Music tracks returned clear audio and no
license families. A historical protected-video fixture was unavailable. None of
these responses can validate protected playback or account entitlement.

## Widevine

Google's documentation requires a license agreement for Widevine products and
services. A native integration needs a supported, licensed CDM and host integration;
the presence of a browser's CDM on the machine does not establish that this
application can redistribute or host it. Windows PlayReady is not a converter for
Widevine-only licenses. No native Widevine implementation is claimed here.

A separate native Media Foundation probe on the test machine checked
`IMFExtendedDRMTypeSupport` and
`IMFMediaEngineClassFactory4::CreateContentDecryptionModuleFactory`. For
`com.widevine.alpha`, the type check returned `NOT_SUPPORTED` and factory
activation returned `0x80700009` (`MF_NOT_SUPPORTED_ERR`). For PlayReady and its
recommendation key system, the type check returned `PROBABLY`, factory activation
succeeded, and the factory accepted the audio type. The probe did not load a
browser CDM, request licenses or change system configuration. Windows has an
extension interface for CDMs; this machine does not expose a Widevine provider
through that interface.

## Verification requirements

A capability result, a successfully generated challenge, a license HTTP response
and actual decrypted playback are separate milestones. Verify real media-clock
advancement, seek and end of stream with an authorized encrypted asset, then verify
the official Music license path for a protected track the account can access.
Do not turn a successful public PlayReady sample into a claim of verified YouTube
DRM compatibility. Do not substitute a clear rendition for an encrypted test.

## Observed native runtime results

The September 17, 2026 Windows test used a standalone .NET 8 application and the
system `MediaPlayer`, with no WebView or browser process. The public asset was
Microsoft's PlayReady-protected Tears of Steel DASH presentation, using the
test service's non-persistent SL150 license policy as listed by Shaka.

- `CheckSupportedHardware(HardwareDRM)` returned true. The separate certificate
  security-level and security-version queries returned `0x8004B8CE`
  (`MSPR_E_HWDRM_NOT_SUPPORTED`). These different results must not be combined
  into a single capability verdict.
- The manifest was accessible and `AdaptiveMediaSource` creation succeeded.
  Playback failed before any individualization or license callback, with
  `SourceNotSupported` / `0xC00D715B` (`MF_E_TOPOLOGY_VERIFICATION_FAILED`).
- Selecting the documented software-protection property did not resolve that
  failure. An audio-only presentation retaining the original encrypted audio
  and protection metadata failed at the same stage.
- Explicit creation of `MediaProtectionPMPServer` succeeded, but the subsequent
  playback attempt still failed with the same topology error.

- A temporary development package, registered with the machine's already enabled
  Developer Mode, established a real package identity. Application-local software
  PlayReady settings were confirmed. Proactive individualization succeeded and
  returned software security level 2000. Media opening still failed with the same
  topology error, before license acquisition. The temporary package and its data
  were removed after testing.

These observations do not establish a universal Windows limitation, or an account
authorization failure: the test had not yet reached a license request.

The newer native Media Foundation route was tested separately, using the public
Microsoft `MediaEngineEMEUWPSample` interfaces. It binds a CDM's PMP server and
trusted input to an `IMFMediaSource`, forwards content-enabling requests through
`IMFContentProtectionManager`, and enables protected content in `IMFMediaEngine`.
This is a native OS pipeline, not a browser host.

In two independent native probes, CDM factory, access, CDM creation and session
creation succeeded. Generating a request failed with `0x8004C3E8`
(`DRM_E_LOGICERR`) before any license challenge. The result persisted with default
and explicit software robustness 2000 configurations, both storage-property
variants, and Microsoft's public CBCS and CENC initialization data. The CENC
input's PSSH/PlayReady Object lengths, system ID and XML were also checked.
The same modern executable was also activated with verified application package
identity, application-local software overrides, AAC robustness 2000 and both
storage-property variants. It read the verified 892-byte CENC initialization data
from an absolute path. Session creation succeeded, but request generation returned
the same `DRM_E_LOGICERR`. It produced no license message and did not reach the
Media Engine playback stage. Temporary package data and CDM stores were removed.

No successful license acquisition, decrypted playback or native YouTube DRM API
is claimed. The results identify the tested machine and integration failures;
they do not prove native PlayReady impossible on Windows in general.

## References

- [Windows PlayReady DRM](https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/playready-client-sdk)
- [Adaptive streaming with PlayReady](https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/adaptive-streaming-with-playready)
- [Widevine overview and licensing](https://developers.google.com/widevine/drm/overview)
- [Widevine integration contact](https://developers.google.com/widevine/contact/support)
- [Native Media Foundation CDM factory](https://learn.microsoft.com/en-us/windows/win32/api/mfmediaengine/nf-mfmediaengine-imfmediaengineclassfactory4-createcontentdecryptionmodulefactory)
- [Microsoft's native PlayReady EME sample](https://github.com/microsoft/media-foundation/tree/master/samples/MediaEngineEMEUWPSample)
- [Shaka's public test-asset definitions](https://github.com/shaka-project/shaka-player/blob/main/demo/common/assets.js)
