/*
 * Decode oracle: runs the original C libsigrokdecode4DSL engine exactly the
 * way DSView's DecoderStack does (pv/data/decoderstack.cpp execute_decode_stack
 * and decode_data, pv/data/decode/decoder.cpp create_decoder_inst) and emits
 * every decoder output as one JSON line. Used as the golden reference for the
 * Rust decoder engine.
 */

#include <glib.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <inttypes.h>
#include "libsigrokdecode.h"

#include "oracle.h"

/* LogicSnapshot geometry (pv/data/logicsnapshot.h). */
#define LEAF_POWER 24
#define LEAF_SAMPLES (1ULL << LEAF_POWER)
/* DecoderStack::MaxChunkSize (pv/data/decoderstack.h). */
#define MAX_CHUNK_SIZE 16384

struct run_ctx {
    oracle_emit emit;
    void *user;
    GString *line;
};

/* ------------------------------------------------------------ JSON helpers */

static void json_str(GString *s, const char *v)
{
    g_string_append_c(s, '"');
    for (const unsigned char *p = (const unsigned char *)(v ? v : ""); *p; p++) {
        switch (*p) {
        case '"':  g_string_append(s, "\\\""); break;
        case '\\': g_string_append(s, "\\\\"); break;
        case '\n': g_string_append(s, "\\n"); break;
        case '\r': g_string_append(s, "\\r"); break;
        case '\t': g_string_append(s, "\\t"); break;
        default:
            if (*p < 0x20)
                g_string_append_printf(s, "\\u%04x", *p);
            else
                g_string_append_c(s, (char)*p);
        }
    }
    g_string_append_c(s, '"');
}

static void emit_line(struct run_ctx *ctx)
{
    ctx->emit(ctx->user, ctx->line->str);
    g_string_truncate(ctx->line, 0);
}

static void emit_error(struct run_ctx *ctx, const char *where, const char *msg)
{
    g_string_append(ctx->line, "{\"t\":\"error\",\"where\":");
    json_str(ctx->line, where);
    g_string_append(ctx->line, ",\"msg\":");
    json_str(ctx->line, msg);
    g_string_append_c(ctx->line, '}');
    emit_line(ctx);
}

static void begin_event(GString *s, const char *type, struct srd_proto_data *pdata)
{
    g_string_append_printf(s, "{\"t\":\"%s\",\"inst\":", type);
    json_str(s, pdata->pdo->di->inst_id);
    g_string_append_printf(s, ",\"ss\":%" PRIu64 ",\"es\":%" PRIu64,
                           pdata->start_sample, pdata->end_sample);
}

/* ------------------------------------------------------------ callbacks */

static void on_annotation(struct srd_proto_data *pdata, void *cb_data)
{
    struct run_ctx *ctx = cb_data;
    struct srd_proto_data_annotation *a = pdata->data;
    GString *s = ctx->line;

    begin_event(s, "ann", pdata);
    g_string_append_printf(s, ",\"cls\":%d,\"type\":%d,\"texts\":[", a->ann_class, a->ann_type);
    for (char **t = a->ann_text; t && *t; t++) {
        if (t != a->ann_text)
            g_string_append_c(s, ',');
        json_str(s, *t);
    }
    g_string_append(s, "]");
    if (a->str_number_hex[0]) {
        g_string_append(s, ",\"hex\":");
        json_str(s, a->str_number_hex);
        g_string_append_printf(s, ",\"num\":%lld", a->numberic_value);
    }
    g_string_append_c(s, '}');
    emit_line(ctx);
}

static void on_binary(struct srd_proto_data *pdata, void *cb_data)
{
    struct run_ctx *ctx = cb_data;
    struct srd_proto_data_binary *b = pdata->data;
    GString *s = ctx->line;

    begin_event(s, "bin", pdata);
    g_string_append_printf(s, ",\"cls\":%d,\"data\":\"", b->bin_class);
    for (uint64_t i = 0; i < b->size; i++)
        g_string_append_printf(s, "%02x", b->data[i]);
    g_string_append(s, "\"}");
    emit_line(ctx);
}

static void on_meta(struct srd_proto_data *pdata, void *cb_data)
{
    struct run_ctx *ctx = cb_data;
    GVariant *v = pdata->data;
    GString *s = ctx->line;

    begin_event(s, "meta", pdata);
    g_string_append(s, ",\"name\":");
    json_str(s, pdata->pdo->meta_name);
    if (g_variant_is_of_type(v, G_VARIANT_TYPE_INT64))
        g_string_append_printf(s, ",\"value\":%" G_GINT64_FORMAT "}", g_variant_get_int64(v));
    else if (g_variant_is_of_type(v, G_VARIANT_TYPE_DOUBLE))
        g_string_append_printf(s, ",\"value\":%.17g}", g_variant_get_double(v));
    else
        g_string_append(s, ",\"value\":null}");
    emit_line(ctx);
}

/* ------------------------------------------------------------ setup */

int oracle_decode_init(const char *decoder_dir, int log_level)
{
    srd_log_level(log_level);
    if (srd_init(decoder_dir) != SRD_OK)
        return -1;
    if (srd_decoder_load_all() != SRD_OK)
        return -2;
    return 0;
}

void oracle_decode_exit(void)
{
    srd_exit();
}

/* Option typing as in StoreSession::load_decoders (storesession.cpp): the
 * GVariant type follows the decoder's default; JSON numbers read as "" when a
 * string is expected (QJsonValue::toString on a number). */
static GHashTable *build_options(const struct srd_decoder *dec, const struct oracle_dec *od)
{
    GHashTable *opts = g_hash_table_new_full(g_str_hash, g_str_equal, g_free,
                                             (GDestroyNotify)g_variant_unref);

    for (const GSList *l = dec->options; l; l = l->next) {
        const struct srd_decoder_option *opt = l->data;
        const struct oracle_opt *o = NULL;

        for (int i = 0; i < od->n_opts; i++) {
            if (strcmp(od->opts[i].key, opt->id) == 0)
                o = &od->opts[i];
        }
        if (o == NULL)
            continue;

        const char *vs = o->is_num ? "" : o->str;
        GVariant *v = NULL;

        if (g_variant_is_of_type(opt->def, G_VARIANT_TYPE("d"))) {
            double d = o->is_num ? o->num : 0;
            if (vs[0])
                d = g_ascii_strtod(vs, NULL);
            v = g_variant_new_double(d);
        }
        else if (g_variant_is_of_type(opt->def, G_VARIANT_TYPE("x"))) {
            /* QJsonValue::toInt() then QString::toInt(): 32-bit. */
            int i = o->is_num ? (int)o->num : 0;
            if (vs[0])
                i = (int)strtol(vs, NULL, 10);
            v = g_variant_new_int64(i);
        }
        else if (g_variant_is_of_type(opt->def, G_VARIANT_TYPE("s"))) {
            v = g_variant_new_string(vs);
        }

        if (v != NULL)
            g_hash_table_replace(opts, g_strdup(opt->id), g_variant_ref_sink(v));
    }
    return opts;
}

/* True when the leaf would be freed by LogicSnapshot::calc_mipmap: no
 * transition between consecutive complete 64-bit words (the first word is
 * compared against its own LSB). */
static gboolean leaf_is_const(const uint8_t *bits, uint64_t leaf_samples)
{
    const uint64_t words = leaf_samples / 64;
    uint64_t last = 0;

    for (uint64_t i = 0; i < words; i++) {
        uint64_t w;
        memcpy(&w, bits + i * 8, 8);
        if (i == 0)
            last = (w & 1) ? ~0ULL : 0ULL;
        if (last ^ w)
            return FALSE;
        last = (w >> 63) ? ~0ULL : 0ULL;
    }
    return TRUE;
}

static gboolean sample_at(const uint8_t *bits, uint64_t i)
{
    return (bits[i / 8] >> (i % 8)) & 1;
}

int oracle_decode_run(const struct oracle_dec *stack, int n_dec, uint64_t samplerate,
                      uint64_t total_samples, const uint8_t *const *signals, int n_signals,
                      uint64_t max_chunk, oracle_emit emit, void *user)
{
    struct run_ctx ctx = { emit, user, g_string_new(NULL) };
    struct srd_session *session = NULL;
    struct srd_decoder_inst *prev = NULL, *bottom = NULL;
    char *error = NULL;
    int ret = 0;

    if (max_chunk == 0)
        max_chunk = MAX_CHUNK_SIZE;
    if (total_samples == 0) {
        emit_error(&ctx, "input", "no samples");
        g_string_free(ctx.line, TRUE);
        return -1;
    }

    srd_session_new(&session);

    for (int d = 0; d < n_dec; d++) {
        const struct srd_decoder *dec = srd_decoder_get_by_id(stack[d].id);
        if (dec == NULL) {
            emit_error(&ctx, "stack", stack[d].id);
            ret = -1;
            goto done;
        }

        GHashTable *opts = build_options(dec, &stack[d]);
        struct srd_decoder_inst *di = srd_inst_new(session, stack[d].id, opts);
        g_hash_table_destroy(opts);
        if (di == NULL) {
            emit_error(&ctx, "inst_new", stack[d].id);
            ret = -1;
            goto done;
        }

        GHashTable *probes = g_hash_table_new_full(g_str_hash, g_str_equal, g_free,
                                                   (GDestroyNotify)g_variant_unref);
        for (int c = 0; c < stack[d].n_ch; c++) {
            if (stack[d].ch_idx[c] < 0 || stack[d].ch_idx[c] >= n_signals ||
                signals[stack[d].ch_idx[c]] == NULL) {
                g_hash_table_destroy(probes);
                emit_error(&ctx, "channels", "decoder channel bound to a missing signal");
                ret = -1;
                goto done;
            }
            g_hash_table_insert(probes, g_strdup(stack[d].ch_id[c]),
                                g_variant_ref_sink(g_variant_new_int32(stack[d].ch_idx[c])));
        }
        srd_inst_channel_set_all(di, probes);
        g_hash_table_destroy(probes);

        if (prev)
            srd_inst_stack(session, prev, di);
        else
            bottom = di;
        prev = di;
    }

    srd_session_metadata_set(session, SRD_CONF_SAMPLERATE, g_variant_new_uint64(samplerate));
    srd_pd_output_callback_add(session, SRD_OUTPUT_ANN, on_annotation, &ctx);
    srd_pd_output_callback_add(session, SRD_OUTPUT_BINARY, on_binary, &ctx);
    srd_pd_output_callback_add(session, SRD_OUTPUT_META, on_meta, &ctx);

    if (srd_session_start(session, &error) != SRD_OK) {
        emit_error(&ctx, "start", error ? error : "srd_session_start failed");
        ret = -1;
        goto done;
    }

    /* DecoderStack::decode_data for a finished capture. */
    {
        const int nch = bottom->dec_num_channels;
        const uint64_t end_index = total_samples - 1;
        const uint8_t **chunk = g_new0(const uint8_t *, nch > 0 ? nch : 1);
        uint8_t *chunk_const = g_new0(uint8_t, nch > 0 ? nch : 1);
        gboolean end_time = FALSE, failed = FALSE;
        uint64_t i = 0;

        while (i <= end_index) {
            uint64_t chunk_end = end_index;

            for (int j = 0; j < nch; j++) {
                int sig = bottom->dec_channelmap[j];
                if (sig == -1) {
                    chunk[j] = NULL;
                    chunk_const[j] = 0;
                    continue;
                }
                /* LogicSnapshot::get_samples: pointer into the leaf, chunk ends at the leaf end. */
                uint64_t leaf = i / LEAF_SAMPLES;
                uint64_t leaf_start = leaf * LEAF_SAMPLES;
                uint64_t leaf_len = MIN(LEAF_SAMPLES, total_samples - leaf_start);
                const uint8_t *leaf_bits = signals[sig] + leaf_start / 8;

                chunk_end = MIN(leaf_start + LEAF_SAMPLES, total_samples);
                chunk[j] = leaf_is_const(leaf_bits, leaf_len) ? NULL : leaf_bits + (i % LEAF_SAMPLES) / 8;
                chunk_const[j] = sample_at(signals[sig], i);
            }

            if (chunk_end >= end_index)
                chunk_end = end_index + 1;
            if (chunk_end - i > max_chunk)
                chunk_end = i + max_chunk;
            end_time = chunk_end > end_index;

            if (srd_session_send(session, i, chunk_end, chunk, chunk_const, chunk_end - i, &error) != SRD_OK) {
                emit_error(&ctx, "send", error ? error : "srd_session_send failed");
                g_free(error);
                error = NULL;
                failed = TRUE;
                ret = -1;
                break;
            }
            i = chunk_end;
        }

        if (!failed && end_time) {
            srd_session_end(session, &error);
            if (error != NULL) {
                emit_error(&ctx, "end", error);
                g_free(error);
                error = NULL;
            }
        }
        g_free(chunk);
        g_free(chunk_const);
    }

done:
    g_free(error);
    srd_session_destroy(session);
    g_string_free(ctx.line, TRUE);
    return ret;
}
