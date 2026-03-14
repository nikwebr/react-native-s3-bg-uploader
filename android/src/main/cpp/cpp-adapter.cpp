#include <jni.h>
#include "S3BgUploaderOnLoad.hpp"

JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM* vm, void*) {
  return margelo::nitro::s3bguploader::initialize(vm);
}
