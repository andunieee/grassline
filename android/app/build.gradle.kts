plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// Set by the release workflow. Without a keystore the release build is signed
// with the debug key, so it can still be installed (but not updated over a
// build signed with another key).
val releaseKeystore = System.getenv("GRASSLINE_KEYSTORE")?.takeIf { it.isNotBlank() }
val versionCodeFromEnv = System.getenv("GRASSLINE_VERSION_CODE")?.toIntOrNull() ?: 1
val versionNameFromEnv = System.getenv("GRASSLINE_VERSION_NAME")?.takeIf { it.isNotBlank() } ?: "0.1.0"

android {
    namespace = "net.grassline.app"
    compileSdk = 35

    defaultConfig {
        applicationId = "net.grassline.app"
        minSdk = 26
        targetSdk = 35
        versionCode = versionCodeFromEnv
        versionName = versionNameFromEnv
        ndk {
            // Rust core built via: just android-libs
            abiFilters += listOf("arm64-v8a", "armeabi-v7a")
        }
    }

    signingConfigs {
        if (releaseKeystore != null) {
            create("release") {
                storeFile = file(releaseKeystore)
                storePassword = System.getenv("GRASSLINE_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("GRASSLINE_KEY_ALIAS")
                keyPassword = System.getenv("GRASSLINE_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = if (releaseKeystore != null) {
                signingConfigs.getByName("release")
            } else {
                signingConfigs.getByName("debug")
            }
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildFeatures {
        compose = true
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2025.01.00")
    implementation(composeBom)
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-core")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.7")
    // 0.30 is the last release built against Kotlin 2.1.0.
    implementation("com.mikepenz:multiplatform-markdown-renderer-m3:0.30.0")
    // QR codes: Google's scanner when the phone has it, else our own.
    implementation("com.google.android.gms:play-services-code-scanner:16.1.0")
    implementation("com.google.zxing:core:3.5.3")
    val camerax = "1.4.1"
    implementation("androidx.camera:camera-camera2:$camerax")
    implementation("androidx.camera:camera-lifecycle:$camerax")
    implementation("androidx.camera:camera-view:$camerax")
}
