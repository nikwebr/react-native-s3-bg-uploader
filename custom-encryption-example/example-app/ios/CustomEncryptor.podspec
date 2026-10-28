Pod::Spec.new do |s|
  s.name             = 'CustomEncryptor'
  s.version          = '0.1.0'
  s.summary          = 'Custom AES-256-GCM encryptor for S3BgUploader'
  s.homepage         = 'https://github.com/example/custom-encryption-example'
  s.license          = { :type => 'MIT' }
  s.author           = { 'Example' => 'example@example.com' }
  s.source           = { :path => '.' }
  s.source_files   = 'CustomEncryptionExample/CustomEncryptor.swift'
  s.ios.deployment_target = '13.0'
  s.vendored_frameworks = 'Libraries/libcustom_encryptor.xcframework'
end