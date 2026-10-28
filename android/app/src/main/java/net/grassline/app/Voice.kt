package net.grassline.app

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.MediaRecorder
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.util.UUID

/** Records a voice message to an AAC file in the cache. */
class VoiceRecorder(private val context: Context) {
    private var recorder: MediaRecorder? = null
    private var file: File? = null

    /** Starts recording; throws when the microphone cannot be used. */
    fun start() {
        cancel()
        val f = File.createTempFile("voice", ".m4a", context.cacheDir)
        @Suppress("DEPRECATION")
        val r = if (Build.VERSION.SDK_INT >= 31) MediaRecorder(context) else MediaRecorder()
        try {
            r.setAudioSource(MediaRecorder.AudioSource.MIC)
            r.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
            r.setAudioEncoder(MediaRecorder.AudioEncoder.AAC)
            // Speech needs little: small files upload fast.
            r.setAudioChannels(1)
            r.setAudioSamplingRate(16_000)
            r.setAudioEncodingBitRate(32_000)
            r.setOutputFile(f.absolutePath)
            r.prepare()
            r.start()
        } catch (e: Exception) {
            r.release()
            f.delete()
            throw e
        }
        recorder = r
        file = f
    }

    /** Stops recording; returns the file, or null when nothing was recorded. */
    fun stop(): File? {
        val r = recorder ?: return null
        val f = file
        recorder = null
        file = null
        // Stopping right after starting throws: there is no audio.
        val ok = runCatching { r.stop() }.isSuccess
        r.release()
        if (!ok) f?.delete()
        return f.takeIf { ok }
    }

    fun cancel() {
        stop()?.delete()
    }
}

/** Turns [audio] into text with an OpenAI-style `audio/transcriptions` endpoint. */
suspend fun transcribe(t: Transcription, audio: File): String = withContext(Dispatchers.IO) {
    val boundary = "grassline-" + UUID.randomUUID()
    val conn = URL(t.url).openConnection() as HttpURLConnection
    try {
        conn.requestMethod = "POST"
        conn.doOutput = true
        conn.connectTimeout = 15_000
        conn.readTimeout = 120_000
        conn.setRequestProperty("Authorization", "Bearer ${t.key}")
        conn.setRequestProperty("Content-Type", "multipart/form-data; boundary=$boundary")
        conn.outputStream.buffered().use { out ->
            fun line(s: String = "") = out.write((s + "\r\n").toByteArray())
            line("--$boundary")
            line("Content-Disposition: form-data; name=\"model\"")
            line()
            line(t.model)
            line("--$boundary")
            line("Content-Disposition: form-data; name=\"file\"; filename=\"voice.m4a\"")
            line("Content-Type: audio/mp4")
            line()
            audio.inputStream().use { it.copyTo(out) }
            line()
            line("--$boundary--")
        }
        val code = conn.responseCode
        val body = (if (code in 200..299) conn.inputStream else conn.errorStream)?.bufferedReader()?.use { it.readText() }.orEmpty()
        val json = runCatching { JSONObject(body) }.getOrNull()
        if (code !in 200..299) {
            // OpenAI and its look-alikes: {"error": {"message": …}}; others say it some other way.
            val why = json?.optJSONObject("error")?.optString("message")?.takeIf { it.isNotBlank() }
                ?: json?.optString("message")?.takeIf { it.isNotBlank() }
                ?: body.take(200).ifBlank { conn.responseMessage }
            throw IOException("speech to text: $code $why")
        }
        json?.optString("text") ?: throw IOException("speech to text: unexpected answer: ${body.take(200)}")
    } finally {
        conn.disconnect()
    }
}

enum class VoicePhase { Idle, Recording, Transcribing }

/** A voice message on its way: recorded, then turned into text for [onText]. */
class VoiceState(private val context: Context, private val scope: CoroutineScope, private val onText: (String) -> Unit) {
    private val recorder = VoiceRecorder(context)
    var phase by mutableStateOf(VoicePhase.Idle)
        private set
    /** When the recording started. */
    var started by mutableLongStateOf(0L)
        private set
    /** What went wrong last. */
    var error by mutableStateOf<String?>(null)
    internal var askPermission: () -> Unit = {}

    fun start() {
        error = null
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) begin()
        else askPermission()
    }

    internal fun begin() {
        try {
            recorder.start()
            started = System.currentTimeMillis()
            phase = VoicePhase.Recording
        } catch (e: Exception) {
            error = "cannot record: ${e.message ?: e}"
        }
    }

    fun cancel() {
        recorder.cancel()
        if (phase == VoicePhase.Recording) phase = VoicePhase.Idle
    }

    /** Stops recording and sends what was said, once [t] turned it into text. */
    fun finish(t: Transcription) {
        val audio = recorder.stop()
        if (audio == null) {
            phase = VoicePhase.Idle
            error = "nothing was recorded"
            return
        }
        phase = VoicePhase.Transcribing
        scope.launch {
            try {
                val text = transcribe(t, audio).trim()
                if (text.isEmpty()) error = "no speech was heard" else onText(text)
            } catch (e: Exception) {
                error = e.message ?: e.toString()
            } finally {
                audio.delete()
                phase = VoicePhase.Idle
            }
        }
    }
}

@Composable
fun rememberVoice(onText: (String) -> Unit): VoiceState {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val send by rememberUpdatedState(onText)
    val voice = remember { VoiceState(context, scope) { send(it) } }
    val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) voice.begin() else voice.error = "Grassline may not use the microphone."
    }
    voice.askPermission = { ask.launch(Manifest.permission.RECORD_AUDIO) }
    DisposableEffect(Unit) { onDispose { voice.cancel() } }
    return voice
}

/** How long it has been recording, as m:ss. */
@Composable
fun RecordingTime(since: Long, modifier: Modifier = Modifier) {
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(since) {
        while (true) {
            now = System.currentTimeMillis()
            delay(250)
        }
    }
    val s = ((now - since) / 1000).coerceAtLeast(0)
    Text("● Recording %d:%02d".format(s / 60, s % 60), modifier, color = MaterialTheme.colorScheme.error)
}
