const path = require('path');
const fs = require('fs');
const HtmlWebpackPlugin = require('html-webpack-plugin');

const rootDir = path.resolve(__dirname, '..');
const localWebEntry = path.resolve(rootDir, 'src/index.web.ts');
const webEntry = fs.existsSync(localWebEntry)
  ? localWebEntry
  : path.resolve(path.dirname(require.resolve('react-native-s3-bg-uploader/package.json')), 'src/index.web.ts');

module.exports = {
  entry: './index.web.js',
  output: {
    path: path.resolve(__dirname, 'web-build'),
    filename: 'bundle.js',
  },
  resolve: {
    alias: {
      'react-native$': 'react-native-web',
      'react-native-s3-bg-uploader': webEntry,
      '@react-native-documents/picker': path.resolve(__dirname, 'stubs/react-native-document-picker.js'),
    },
    extensions: ['.web.tsx', '.web.ts', '.web.js', '.tsx', '.ts', '.js', '.json'],
  },
  module: {
    rules: [
      {
        test: /\.[jt]sx?$/,
        use: {
          loader: 'babel-loader',
          options: {
            presets: [
              ['@babel/preset-env', { targets: { esmodules: true } }],
              ['@babel/preset-react', { runtime: 'automatic' }],
              '@babel/preset-typescript',
            ],
          },
        },
        exclude: /node_modules\/(?!(react-native-web|react-native-safe-area-context|react-native-s3-bg-uploader)\/).*/,
      },
    ],
  },
  plugins: [
    new HtmlWebpackPlugin({
      template: './web/index.html',
    }),
  ],
  devServer: {
    port: 8080,
    open: true,
    hot: true,
  },
};
