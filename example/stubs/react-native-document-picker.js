// Stub for react-native-document-picker on web.
// On web, Platform.OS === 'web' is always true, so this code path is never reached.
export const pickSingle = () => Promise.reject(new Error('Not available on web'));
export const isCancel = () => false;
export const types = {};
