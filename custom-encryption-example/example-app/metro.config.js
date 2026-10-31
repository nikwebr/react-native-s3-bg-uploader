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
  },
};

module.exports = mergeConfig(getDefaultConfig(__dirname), config);
