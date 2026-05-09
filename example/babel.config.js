const fs = require('fs');
const path = require('path');

const localPkg = path.join(__dirname, '../package.json');
const isMonorepo = fs.existsSync(localPkg);
const pak = isMonorepo ? require('../package.json') : require('react-native-s3-bg-uploader/package.json');
const pkgRoot = isMonorepo
  ? path.join(__dirname, '..')
  : path.dirname(require.resolve('react-native-s3-bg-uploader/package.json'));

module.exports = api => {
  api.cache(true);
  return {
    presets: ['module:@react-native/babel-preset'],
    plugins: [
      [
        'module-resolver',
        {
          extensions: ['.js', '.ts', '.json', '.jsx', '.tsx'],
          alias: {
            [pak.name]: path.join(pkgRoot, pak.source),
          },
        },
      ],
    ],
  };
};
