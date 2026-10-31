// Stub for @react-native-documents/picker on web.
// Platform.OS === 'web' is always true there, so App.tsx never calls pick();
// it uses an <input type="file"> instead.
export const pick = () => Promise.reject(new Error('Not available on web'));
export const isCancel = () => false;
export const types = {};
