const { getDefaultConfig, mergeConfig } = require('@react-native/metro-config');
const path = require('path');
const fs = require('fs');

const root = path.resolve(__dirname, '..');
// Resolve symlink to its real path so Metro can watch and resolve it
const packageSymlink = path.resolve(__dirname, 'node_modules/react-native-s3-bg-uploader');
const packageRoot = fs.realpathSync(packageSymlink);

/**
 * Metro configuration
 * https://facebook.github.io/docs/configuration
 *
 * @type {import('metro-config').MetroConfig}
 */
const config = {
  watchFolders: [root, packageRoot],
  resolver: {
    // Follow symlinks so the linked local package resolves correctly
    unstable_enableSymlinks: true,
    nodeModulesPaths: [path.resolve(__dirname, 'node_modules')],
    ...(process.env.E2E === '1' ? {
      disableHierarchicalLookup: true,
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
    } : {}),
  },
};

module.exports = mergeConfig(getDefaultConfig(__dirname), config);
