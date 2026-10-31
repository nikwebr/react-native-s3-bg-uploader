import { AppRegistry } from 'react-native';
import App from './App';
import { name as appName } from './app.json';

AppRegistry.registerComponent(appName, () => App);

// react-native-web needs an explicit mount (index.js only registers for native).
const rootTag = document.getElementById('root');
AppRegistry.runApplication(appName, { rootTag });
