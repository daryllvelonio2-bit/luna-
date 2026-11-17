package ai.luna.mobile.ui

import ai.luna.mobile.theme.LunaTheme
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import java.text.NumberFormat
import java.util.Locale

@Composable
fun LunaCrescentMark(
    modifier: Modifier = Modifier,
    size: Dp = 28.dp,
    tint: Color = LunaTheme.colors.coral,
) {
    Canvas(modifier = modifier.size(size)) {
        val w = this.size.width
        val h = this.size.height
        val path = Path().apply {
            addOval(
                androidx.compose.ui.geometry.Rect(
                    left = w * 0.14f,
                    top = h * 0.12f,
                    right = w * 0.86f,
                    bottom = h * 0.88f,
                ),
            )
        }
        val cutout = Path().apply {
            addOval(
                androidx.compose.ui.geometry.Rect(
                    left = w * 0.36f,
                    top = h * 0.06f,
                    right = w * 0.98f,
                    bottom = h * 0.76f,
                ),
            )
        }
        val crescent = Path().apply {
            op(path, cutout, androidx.compose.ui.graphics.PathOperation.Difference)
        }
        drawPath(crescent, color = tint)
    }
}

@Composable
fun ContextTelemetryPill(
    tokensUsed: Int,
    contextWindow: Int,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = LunaTheme.colors
    val ratio = if (contextWindow > 0 && tokensUsed > 0) {
        (tokensUsed.toFloat() / contextWindow.toFloat()).coerceIn(0f, 1f)
    } else {
        0f
    }
    val pct = (ratio * 100f).toInt().coerceIn(0, 100)
    val gaugeColor = when {
        pct >= 75 -> colors.coral
        pct >= 50 -> colors.amber
        tokensUsed > 0 -> colors.celestial
        else -> colors.inkMuted
    }

    Surface(
        onClick = onClick,
        color = colors.surface,
        shape = RoundedCornerShape(999.dp),
        border = BorderStroke(1.dp, colors.hairline),
        modifier = modifier.height(36.dp),
    ) {
        Row(
            modifier = Modifier.padding(horizontal = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Canvas(modifier = Modifier.size(16.dp)) {
                val strokeWidth = 2.4.dp.toPx()
                val arcSize = Size(size.width - strokeWidth, size.height - strokeWidth)
                val topLeft = Offset(strokeWidth / 2f, strokeWidth / 2f)
                drawArc(
                    color = colors.outline,
                    startAngle = -90f,
                    sweepAngle = 360f,
                    useCenter = false,
                    topLeft = topLeft,
                    size = arcSize,
                    style = Stroke(width = strokeWidth, cap = StrokeCap.Round),
                )
                if (ratio > 0f) {
                    drawArc(
                        color = gaugeColor,
                        startAngle = -90f,
                        sweepAngle = 360f * ratio.coerceAtLeast(0.05f),
                        useCenter = false,
                        topLeft = topLeft,
                        size = arcSize,
                        style = Stroke(width = strokeWidth, cap = StrokeCap.Round),
                    )
                }
            }
            Text(
                text = if (tokensUsed > 0) "$pct%" else "ctx",
                style = MaterialTheme.typography.labelMedium.copy(
                    color = if (tokensUsed > 0) colors.ink else colors.inkMuted,
                    fontFeatureSettings = "tnum",
                ),
            )
        }
    }
}

@Composable
fun StatusPill(
    label: String,
    foreground: Color,
    background: Color,
    modifier: Modifier = Modifier,
) {
    Box(
        modifier = modifier
            .clip(RoundedCornerShape(999.dp))
            .background(background)
            .padding(horizontal = 8.dp, vertical = 3.dp),
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall.copy(
                color = foreground,
                fontWeight = FontWeight.SemiBold,
            ),
        )
    }
}

@Composable
fun SectionHeader(
    title: String,
    modifier: Modifier = Modifier,
    trailing: (@Composable () -> Unit)? = null,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .padding(top = 12.dp, bottom = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = title,
            style = MaterialTheme.typography.titleSmall.copy(
                color = LunaTheme.colors.inkSecondary,
            ),
            modifier = Modifier.weight(1f),
        )
        trailing?.invoke()
    }
}

@Composable
fun ConfirmDialog(
    title: String,
    body: String,
    confirmLabel: String,
    destructive: Boolean = false,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    val colors = LunaTheme.colors
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = colors.surfaceLow,
        titleContentColor = colors.ink,
        textContentColor = colors.inkSecondary,
        title = { Text(title, style = MaterialTheme.typography.titleLarge) },
        text = { Text(body, style = MaterialTheme.typography.bodyMedium) },
        confirmButton = {
            Button(
                onClick = onConfirm,
                colors = ButtonDefaults.buttonColors(
                    containerColor = if (destructive) colors.bad else colors.coral,
                    contentColor = if (destructive) Color.White else colors.onCoral,
                ),
            ) {
                Text(confirmLabel)
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("Cancel", color = colors.inkSecondary)
            }
        },
    )
}

fun formatTokenCount(count: Int): String =
    NumberFormat.getNumberInstance(Locale.US).format(count)

fun formatCompactTokens(count: Int): String = when {
    count >= 1_000_000 -> String.format(Locale.US, "%.1fM", count / 1_000_000f)
    count >= 1_000 -> String.format(Locale.US, "%.1fk", count / 1_000f)
    else -> count.toString()
}
