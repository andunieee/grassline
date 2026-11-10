import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// Release signing. In priority order:
//   1. environment variables (the release workflow sets GRASSLINE_*; local
//      shells can set either name, e.g. by sourcing ~/android-release/env.sh)
//   2. an optional shared properties file, by default
//      ~/android-release/signing.properties, which other local Android
//      projects can read too (override its path with ANDROID_SIGNING_PROPERTIES)
//   3. the debug key, so assembleRelease still works without a key
val signingPropertiesFile = File(
    System.getenv("ANDROID_SIGNING_PROPERTIES")
        ?: "${System.getProperty("user.home")}/android-release/signing.properties",
)
val sharedSigning = Properties().apply {
    if (signingPropertiesFile.isFile) {
        signingPropertiesFile.inputStream().use { load(it) }
    }
}

fun signingValue(envNames: List<String>, propertyName: String): String? =
    envNames.firstNotNullOfOrNull { System.getenv(it)?.takeIf(String::isNotBlank) }
        ?: sharedSigning.getProperty(propertyName)?.takeIf(String::isNotBlank)

val releaseKeystore = signingValue(listOf("GRASSLINE_KEYSTORE", "ANDROID_KEYSTORE"), "storeFile")
    ?.let { path ->
        val file = File(path)
        if (file.isAbsolute) file else File(signingPropertiesFile.absoluteFile.parentFile, path)
    }
val releaseKeystorePassword =
    signingValue(listOf("GRASSLINE_KEYSTORE_PASSWORD", "ANDROID_KEYSTORE_PASSWORD"), "storePassword")
val releaseKeyAlias = signingValue(listOf("GRASSLINE_KEY_ALIAS", "ANDROID_KEY_ALIAS"), "keyAlias")
val releaseKeyPassword =
    signingValue(listOf("GRASSLINE_KEY_PASSWORD", "ANDROID_KEY_PASSWORD"), "keyPassword")
val hasReleaseSigning = releaseKeystore?.isFile == true &&
    releaseKeystorePassword != null && releaseKeyAlias != null && releaseKeyPassword != null

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
        if (hasReleaseSigning) {
            create("release") {
                storeFile = releaseKeystore
                storePassword = releaseKeystorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = if (hasReleaseSigning) {
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
