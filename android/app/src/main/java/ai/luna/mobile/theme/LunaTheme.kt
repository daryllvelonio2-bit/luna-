package ai.luna.mobile.theme

import android.content.Context
import android.provider.Settings
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

enum class LunaThemeMode(val label: String) {
    System("System"),
    Dark("Dark"),
    Light("Light"),
}

@Immutable
data class LunaColors(
    val canvas: Color,
    val surfaceLow: Color,
    val surface: Color,
    val surfaceHigh: Color,
    val codeBg: Color,
    val outline: Color,
    val hairline: Color,
    val ink: Color,
    val inkSecondary: Color,
    val inkMuted: Color,
    val coral: Color,
    val onCoral: Color,
    val coralContainer: Color,
    val celestial: Color,
    val celestialContainer: Color,
    val amber: Color,
    val amberContainer: Color,
    val ok: Color,
    val okContainer: Color,
    val bad: Color,
    val badContainer: Color,
    val noteAccent: Color,
    val isDark: Boolean,
)

val DarkLunaColors = LunaColors(
    canvas = Color(0xFF0C0D10),
    surfaceLow = Color(0xFF13151B),
    surface = Color(0xFF191C24),
    surfaceHigh = Color(0xFF222631),
    codeBg = Color(0xFF111319),
    outline = Color(0xFF2D3342),
    hairline = Color(0xFF1F2430),
    ink = Color(0xFFECEFF4),
    inkSecondary = Color(0xFFA7B0BF),
    inkMuted = Color(0xFF6F7889),
    coral = Color(0xFFEA7652),
    onCoral = Color(0xFF170804),
    coralContainer = Color(0xFF331C15),
    celestial = Color(0xFF6AA6E8),
    celestialContainer = Color(0xFF162436),
    amber = Color(0xFFE5B567),
    amberContainer = Color(0xFF2E2413),
    ok = Color(0xFF68C282),
    okContainer = Color(0xFF14281B),
    bad = Color(0xFFE06C75),
    badContainer = Color(0xFF2F161A),
    noteAccent = Color(0xFFD28CE6),
    isDark = true,
)

val LightLunaColors = LunaColors(
    canvas = Color(0xFFF7F7FA),
    surfaceLow = Color(0xFFFFFFFF),
    surface = Color(0xFFEFEFF4),
    surfaceHigh = Color(0xFFE4E6EE),
    codeBg = Color(0xFFECEEF3),
    outline = Color(0xFFD0D5E0),
    hairline = Color(0xFFE5E8F0),
    ink = Color(0xFF14171F),
    inkSecondary = Color(0xFF4D5566),
    inkMuted = Color(0xFF7B8496),
    coral = Color(0xFFD05430),
    onCoral = Color(0xFFFFFFFF),
    coralContainer = Color(0xFFFCE7E0),
    celestial = Color(0xFF2A6EC2),
    celestialContainer = Color(0xFFE2EEFA),
    amber = Color(0xFFB07212),
    amberContainer = Color(0xFFFAF0DD),
    ok = Color(0xFF228644),
    okContainer = Color(0xFFE2F4E8),
    bad = Color(0xFFC4303C),
    badContainer = Color(0xFFFBE5E7),
    noteAccent = Color(0xFF9438B0),
    isDark = false,
)

val LocalLunaColors = staticCompositionLocalOf { DarkLunaColors }
val LocalReduceMotion = staticCompositionLocalOf { false }

object LunaTheme {
    val colors: LunaColors
        @Composable
        @ReadOnlyComposable
        get() = LocalLunaColors.current

    val reduceMotion: Boolean
        @Composable
        @ReadOnlyComposable
        get() = LocalReduceMotion.current
}

private fun LunaColors.toMaterialColorScheme(): ColorScheme = if (isDark) {
    darkColorScheme(
        primary = coral,
        onPrimary = onCoral,
        primaryContainer = coralContainer,
        onPrimaryContainer = coral,
        secondary = celestial,
        onSecondary = Color(0xFF0A1626),
        secondaryContainer = celestialContainer,
        onSecondaryContainer = celestial,
        tertiary = noteAccent,
        background = canvas,
        onBackground = ink,
        surface = surfaceLow,
        onSurface = ink,
        surfaceVariant = surface,
        onSurfaceVariant = inkSecondary,
        surfaceContainerLowest = canvas,
        surfaceContainerLow = surfaceLow,
        surfaceContainer = surface,
        surfaceContainerHigh = surfaceHigh,
        surfaceContainerHighest = surfaceHigh,
        outline = outline,
        outlineVariant = hairline,
        error = bad,
        onError = Color(0xFF26080B),
        errorContainer = badContainer,
        onErrorContainer = bad,
    )
} else {
    lightColorScheme(
        primary = coral,
        onPrimary = onCoral,
        primaryContainer = coralContainer,
        onPrimaryContainer = coral,
        secondary = celestial,
        onSecondary = Color.White,
        secondaryContainer = celestialContainer,
        onSecondaryContainer = celestial,
        tertiary = noteAccent,
        background = canvas,
        onBackground = ink,
        surface = surfaceLow,
        onSurface = ink,
        surfaceVariant = surface,
        onSurfaceVariant = inkSecondary,
        surfaceContainerLowest = surfaceLow,
        surfaceContainerLow = canvas,
        surfaceContainer = surface,
        surfaceContainerHigh = surfaceHigh,
        surfaceContainerHighest = surfaceHigh,
        outline = outline,
        outlineVariant = hairline,
        error = bad,
        onError = Color.White,
        errorContainer = badContainer,
        onErrorContainer = bad,
    )
}

private val LunaTypography = Typography(
    headlineSmall = TextStyle(
        fontFamily = FontFamily.SansSerif,
        fontWeight = FontWeight.SemiBold,
        fontSize = 22.sp,
        lineHeight = 28.sp,
        letterSpacing = (-0.2).sp,
    ),
    titleLarge = TextStyle(
        fontFamily = FontFamily.SansSerif,
        fontWeight = FontWeight.SemiBold,
        fontSize = 18.sp,
        lineHeight = 24.sp,
    ),
    titleMedium = TextStyle(
        fontFamily = FontFamily.SansSerif,
        fontWeight = FontWeight.SemiBold,
        fontSize = 16.sp,
        lineHeight = 22.sp,
    ),
    titleSmall = TextStyle(
        fontFamily = FontFamily.SansSerif,
        fontWeight = FontWeight.SemiBold,
        fontSize = 14.sp,
        lineHeight = 20.sp,
    ),
    bodyLarge = TextStyle(
        fontFamily = FontFamily.SansSerif,
        fontWeight = FontWeight.Normal,
        fontSize = 15.sp,
        lineHeight = 22.sp,
    ),
    bodyMedium = TextStyle(
        fontFamily = FontFamily.SansSerif,
        fontWeight = FontWeight.Normal,
        fontSize = 14.sp,
        lineHeight = 20.sp,
    ),
    bodySmall = TextStyle(
        fontFamily = FontFamily.SansSerif,
        fontWeight = FontWeight.Normal,
        fontSize = 12.sp,
        lineHeight = 17.sp,
    ),
    labelLarge = TextStyle(
        fontFamily = FontFamily.SansSerif,
        fontWeight = FontWeight.Medium,
        fontSize = 14.sp,
        lineHeight = 20.sp,
    ),
    labelMedium = TextStyle(
        fontFamily = FontFamily.SansSerif,
        fontWeight = FontWeight.Medium,
        fontSize = 12.sp,
        lineHeight = 16.sp,
    ),
    labelSmall = TextStyle(
        fontFamily = FontFamily.SansSerif,
        fontWeight = FontWeight.Medium,
        fontSize = 11.sp,
        lineHeight = 14.sp,
    ),
)

private val LunaShapes = Shapes(
    small = RoundedCornerShape(10.dp),
    medium = RoundedCornerShape(14.dp),
    large = RoundedCornerShape(20.dp),
    extraLarge = RoundedCornerShape(26.dp),
)

fun isSystemReducedMotion(context: Context): Boolean =
    runCatching {
        Settings.Global.getFloat(
            context.contentResolver,
            Settings.Global.ANIMATOR_DURATION_SCALE,
            1f,
        ) == 0f
    }.getOrDefault(false)

fun ComponentActivity.applySystemBars(dark: Boolean) {
    val transparent = android.graphics.Color.TRANSPARENT
    val lightScrim = android.graphics.Color.argb(0xe6, 0xFF, 0xFF, 0xFF)
    val darkScrim = android.graphics.Color.argb(0x80, 0x1b, 0x1b, 0x1b)
    enableEdgeToEdge(
        statusBarStyle = if (dark) {
            SystemBarStyle.dark(transparent)
        } else {
            SystemBarStyle.light(transparent, transparent)
        },
        navigationBarStyle = if (dark) {
            SystemBarStyle.dark(darkScrim)
        } else {
            SystemBarStyle.light(lightScrim, darkScrim)
        },
    )
}

@Composable
fun LunaMobileTheme(
    themeMode: LunaThemeMode = LunaThemeMode.System,
    content: @Composable () -> Unit,
) {
    val context = LocalContext.current
    val systemDark = isSystemInDarkTheme()
    val useDark = when (themeMode) {
        LunaThemeMode.System -> systemDark
        LunaThemeMode.Dark -> true
        LunaThemeMode.Light -> false
    }
    val lunaColors = if (useDark) DarkLunaColors else LightLunaColors
    val colorScheme = remember(lunaColors) { lunaColors.toMaterialColorScheme() }
    val reduceMotion = remember(context) { isSystemReducedMotion(context) }

    CompositionLocalProvider(
        LocalLunaColors provides lunaColors,
        LocalReduceMotion provides reduceMotion,
    ) {
        MaterialTheme(
            colorScheme = colorScheme,
            typography = LunaTypography,
            shapes = LunaShapes,
            content = content,
        )
    }
}
