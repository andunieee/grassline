package net.grassline.app

import android.content.Context
import android.graphics.Color as AndroidColor
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.LocalActivity
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.*
import androidx.compose.runtime.*

enum class ThemeMode(val label: String) { System("System theme"), Light("Light theme"), Dark("Dark theme") }

/** The theme picked in the menu, kept across launches. */
class ThemeSetting(context: Context) {
    private val prefs = context.getSharedPreferences("settings", Context.MODE_PRIVATE)
    var mode by mutableStateOf(ThemeMode.entries.firstOrNull { it.name == prefs.getString("theme", null) } ?: ThemeMode.System)
        private set

    fun set(m: ThemeMode) {
        mode = m
        prefs.edit().putString("theme", m.name).apply()
    }
}

val LocalThemeSetting = staticCompositionLocalOf<ThemeSetting> { error("no theme setting") }

@Composable
fun GrasslineTheme(setting: ThemeSetting, content: @Composable () -> Unit) {
    val dark = when (setting.mode) {
        ThemeMode.System -> isSystemInDarkTheme()
        ThemeMode.Light -> false
        ThemeMode.Dark -> true
    }
    // The system bars' icons follow the app's theme, not the system's.
    val activity = LocalActivity.current as? ComponentActivity
    DisposableEffect(activity, dark) {
        val style = SystemBarStyle.auto(AndroidColor.TRANSPARENT, AndroidColor.TRANSPARENT) { dark }
        activity?.enableEdgeToEdge(style, style)
        onDispose {}
    }
    CompositionLocalProvider(LocalThemeSetting provides setting) {
        MaterialTheme(colorScheme = if (dark) darkColorScheme() else lightColorScheme(), content = content)
    }
}
