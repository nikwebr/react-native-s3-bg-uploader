const fs = require('fs')
const path = require('path')

const localPkg = path.join(__dirname, '../package.json')
const isMonorepo = fs.existsSync(localPkg)
const pkg = isMonorepo ? require('../package.json') : require('react-native-s3-bg-uploader/package.json')
const pkgRoot = isMonorepo
  ? path.join(__dirname, '..')
  : path.dirname(require.resolve('react-native-s3-bg-uploader/package.json'))

/**
 * @type {import('@react-native-community/cli-types').Config}
 */
module.exports = {
    project: {
        ios: {
            automaticPodsInstallation: true,
        },
    },
    dependencies: {
        [pkg.name]: {
            root: pkgRoot,
        },
    },
}
