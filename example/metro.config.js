const { getDefaultConfig, mergeConfig } = require('@react-native/metro-config');
const path = require('path');
const root = path.resolve(__dirname, '..');

/**
 * Metro configuration
 * https://facebook.github.io/metro/docs/configuration
 *
 * @type {import('metro-config').MetroConfig}
 */
const config = {
  watchFolders: [root],
  ...(process.env.E2E === '1' ? {
    resolver: {
      blockList: [
        /[/\\]e2e[/\\](?:\.venv|\.tools|artifacts)[/\\]/,
        /[/\\](?:uploader|custom-encryptor-rust|tools[/\\]decrypt)[/\\]target[/\\]/,
        /[/\\](?:(?:example|example-app)[/\\](?:ios|android)|android)[/\\](?:build|Pods|\.cxx|\.gradle)[/\\]/,
        /[/\\](?:example|example-app)[/\\]web-build[/\\]/,
      ],
      resolveRequest(context, moduleName, platform) {
        if (moduleName === './App' &&
            context.originModulePath === path.join(__dirname, 'index.js')) {
          return { type: 'sourceFile', filePath: path.join(__dirname, 'e2e/TestApp.tsx') };
        }
        return context.resolveRequest(context, moduleName, platform);
      },
    },
  } : {}),
};

module.exports = mergeConfig(getDefaultConfig(__dirname), config);