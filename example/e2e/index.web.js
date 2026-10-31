import { AppRegistry } from 'react-native';
import TestApp from './TestApp';
import { name as appName } from '../app.json';

AppRegistry.registerComponent(appName, () => TestApp);
AppRegistry.runApplication(appName, { rootTag: document.getElementById('root') });
