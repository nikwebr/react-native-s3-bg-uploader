## [2.0.0](https://github.com/nikwebr/react-native-s3-bg-uploader/compare/v1.3.1...v2.0.0) (2026-11-16)

### ⚠ BREAKING CHANGES

* **api:** `setConfig()` takes `cancelApi` as its fourth argument.
Callers that passed `sendFileHash` positionally as the fourth argument must
move it to the fifth position; `false` in the fourth position is now read as
a cancel endpoint.

### ✨ Features

* **upload:** abort cancelled multipart uploads on the backend ([9719294](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/9719294707f0bb60d22ded2a4673ccf99a8a47ff))
* **upload:** add hash salt for collision resolution ([cc88564](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/cc88564a6e01ebbab9294d1b74509893d0d25a47)), closes [#62](https://github.com/nikwebr/react-native-s3-bg-uploader/issues/62)
* **web:** cancel parts on pause ([4367e91](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/4367e918ec1ae074d37dcf7b4d51634ae70d0540))

### 🐛 Bug Fixes

* **ios:** keep BGContinuedProcessingTask open when upload is completing ([353ea78](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/353ea78b56d0a67f978ab1562e4cb77144c05cb6))
* **ios:** render background task text before first progress update ([3f603fb](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/3f603fb22f6110a1d646d261a76dc73bd209e2a1))
* **upload-engine:** pause reliably cancels all bytes currently in-transfer ([4306e2e](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/4306e2e4148bf54aaea966a0458de70c58db929d))
* **upload:** reduce request timeout from 300s to 30s ([f9e9382](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/f9e93825999f64118d021600676c876df792496f))
* **upload:** restore upload behavior from 353ea78 ([11fa09c](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/11fa09c10bde9f4cdfdb7cdd8d494bfe5246ba1a))
* **web:** add XHR timeout so network recovery retries can proceed ([c640e97](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/c640e97ec87b219e720cbbca7dc923cb01a3f478))

### 🔄 Code Refactors

* **api:** take cancelApi before sendFileHash in setConfig ([0b70dd2](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/0b70dd262f245e80e80b8d5c66aace068678322d))

### 📚 Documentation

* **e2e:** document hash salt coverage ([d307f57](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/d307f578a19a898eebbdc0eb5be7573820da5844))

### 🛠️ Other changes

* **ci:** accept zeroed progress in e2e network recovery test ([8581896](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/85818962754e40a22ff01ce31d21dec32680ede2))
* **ci:** boot the Android emulator without snapshots ([e6d8329](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/e6d8329698f19ce31f17663a3972e616f5918d7e))
* **ci:** move agent automation to Paseo Hub role labels ([#153](https://github.com/nikwebr/react-native-s3-bg-uploader/issues/153)) ([2c6ae51](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/2c6ae510062c56498033a5395409681cec0b44c0))
* **ci:** notify Paseo Hub on CI failures ([e58c33f](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/e58c33f487627ed991a4ac32d3cc58599d99162d))
* **ci:** recover repeated adb startup disconnects in Android e2e ([5b572e5](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/5b572e5bb3b8f3c202177b5464d2a8d29014239c))
* **ci:** reduce e2e network failure delay and increase test timeouts ([f4ba744](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/f4ba744229dbee42bebd3a9d5c809fcacaf20a49))
* **ci:** restore adb reverse ports before startup retry ([c2227f1](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/c2227f1ad78244166797c0fbe877ffcb141a74ab))
* **ci:** restore and correct cross-platform e2e checks ([8fbec15](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/8fbec151d3591b91ff648ac1162d720b059315d8))
* **ci:** run Paseo Hub agent as codex on deepseek ([e45f473](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/e45f47349c17afd5bfd2146efb96c69bacb2f194))
* **ci:** trigger Hub on unlabeled agent-serviceuser mentions ([a70d5b2](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/a70d5b2d3614a25e50ff98016bb9ad6f97252e3a))

## [1.3.1](https://github.com/nikwebr/react-native-s3-bg-uploader/compare/v1.3.0...v1.3.1) (2026-11-01)

### 🐛 Bug Fixes

* **encryption:** let the backend derive the encrypted size ([cad2249](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/cad2249bc7aff4a3b324a567b01f3612ad3e50cc))
* **encryption:** report ciphertext sizes for encrypted uploads ([139bc97](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/139bc97aab8be04764158cd3e7494fad69e4ea6e))

### 📚 Documentation

* **agents:** add commit-type and release rules for coding agents ([b2b9fce](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/b2b9fceb2c421416ca70141a90f7488cbd292b42))

## [1.3.0](https://github.com/nikwebr/react-native-s3-bg-uploader/compare/v1.2.0...v1.3.0) (2026-11-01)

### ✨ Features

* **ci:** allow manual docs checks for merged PRs ([6ab371e](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/6ab371e3e3f781a3a5bda2237ab446962db6511d))

### 🐛 Bug Fixes

* **ci:** use GH_TOKEN consistently for docs workflow ([f75ccd8](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/f75ccd84147d9a54d75ff740f97037cfa440aae0))

## [1.2.0](https://github.com/nikwebr/react-native-s3-bg-uploader/compare/v1.1.3...v1.2.0) (2026-10-31)

### ✨ Features

* add custom encryption example for iOS Android and web ([1b683f7](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/1b683f71332445bc4f070726c0494515889d82b0))

### 🐛 Bug Fixes

* **ci:** make CodeQL simulator build device-independent ([cb06b3f](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/cb06b3fdc6806657252b1dd1781686678b98bc3a))
* **encryption:** correct documentation logging and footer defaults ([32660ac](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/32660ac0f677348d1532ed7707254bd8828420a7))
* **encryption:** guard resumed uploads and clean up web encryptors ([0f9ed4e](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/0f9ed4ee4e91213a8ec80167b794b73c5328d3e1))
* **encryption:** register encryption atomically and reject non-ASCII hex ([c036605](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/c0366059f3db5a16b2c3fc62c8afe8cc331e2407))
* **wasm:** install wasm-bindgen CLI version matching Cargo.lock ([4441cea](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/4441cea1304bc53aef87d9f0fef1f728a24d3ffc))

## [1.1.3](https://github.com/nikwebr/react-native-s3-bg-uploader/compare/v1.1.2...v1.1.3) (2026-05-12)

### 🐛 Bug Fixes

* bump versions regarding vulnerabilities ([b17b8cf](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/b17b8cf9ffc197b283b9b5111f535357dc4dfeac))

## [1.1.2](https://github.com/nikwebr/react-native-s3-bg-uploader/compare/v1.1.1...v1.1.2) (2026-05-09)

### 🐛 Bug Fixes

* getProgress now takes fileHash as a parameter instead of fileKey, translation for example app, adapted readme ([02668ac](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/02668acebe4a33debdba6d7fd80c1f9d9ae18465))

## [1.1.1](https://github.com/nikwebr/react-native-s3-bg-uploader/compare/v1.1.0...v1.1.1) (2026-05-06)

### 🐛 Bug Fixes

* minor improvements: Readme, package.json & example app setup ([ecc7d6e](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/ecc7d6e096c7a5a0e8d4c58e9249e34f9bbcc4bd))

## [1.1.0](https://github.com/nikwebr/react-native-s3-bg-uploader/compare/v1.0.0...v1.1.0) (2026-05-06)

### ✨ Features

* **pencil:** initial release ([9f5c21e](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/9f5c21e787b0a018f1263371e1ddbdeb4c322b3c))

### 🛠️ Other changes

* **release:** 1.1.0 [skip ci] ([1f447e3](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/1f447e3c6452c491112917bd39592e45a8391f6e))

## [1.1.0](https://github.com/nikwebr/react-native-s3-bg-uploader/compare/v1.0.0...v1.1.0) (2026-05-06)

### ✨ Features

* **pencil:** initial release ([9f5c21e](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/9f5c21e787b0a018f1263371e1ddbdeb4c322b3c))

## 1.0.0 (2026-05-05)

### ✨ Features

* initial release ([c8e694f](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/c8e694fea600615b4a936ce7c409cc3f7ad97aa4))

### 🛠️ Other changes

* **release:** 1.0.0 [skip ci] ([dfbbb53](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/dfbbb5309a7055674e97ffb239956abb29d300b2))

## 1.0.0 (2026-05-05)

### ✨ Features

* initial release ([c8e694f](https://github.com/nikwebr/react-native-s3-bg-uploader/commit/c8e694fea600615b4a936ce7c409cc3f7ad97aa4))
