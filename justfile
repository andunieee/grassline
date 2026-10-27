# List recipes
export ANDROID_HOME := env_var_or_default('ANDROID_HOME', '/opt/android-sdk')
export ANDROID_NDK_HOME := env_var_or_default('ANDROID_NDK_HOME', ANDROID_HOME / 'ndk' / '26.3.11579264')

default:
    @just --list

# Run the daemon (admin interface on http://127.0.0.1:7878, open it with a hyperb browser)
daemon *args:
    cargo run -p grassline-daemon -- {{args}}

# Install the daemon for this user and enable it as a systemd user service
daemon-install:
    cargo install --path crates/daemon --locked
    install -Dm644 deploy/grasslined.service ~/.config/systemd/user/grasslined.service
    systemctl --user daemon-reload
    systemctl --user enable --now grasslined
    @echo "grasslined is running; admin interface on hyperb://127.0.0.1:7878/"

# Build the Rust core of the Android app
android-libs:
    cargo ndk -t arm64-v8a -t armeabi-v7a -o android/app/src/main/jniLibs build --release -p grassline-mobile

# Build the debug APK
android-apk: android-libs
    android/gradlew -p android assembleDebug

# Build and install the app on every device connected through adb
android: android-libs
    android/gradlew -p android installDebug

test:
    cargo test --workspace
