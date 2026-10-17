/*
 * Output modules backed by the Rust core (crates/dsview-formats export),
 * used instead of csv.c/vcd.c when DSVIEW_IMPL_FORMATS=rust (or
 * DSVIEW_IMPL=rust). Transitional: removed with the C output modules.
 */

#ifdef DSVIEW_RUST_CORE

#include "../libsigrok-internal.h"
#include <glib.h>
#include <string.h>
#include <time.h>
#include "../log.h"
#include "dsview_ffi.h"

#undef LOG_PREFIX
#define LOG_PREFIX "rs-output: "

struct rs_context {
    struct dsv_export *exporter;
    int strings_set;
    uint64_t enabled_of_type;
    uint64_t all_channels;
};

static int kind_code(int sr_type)
{
    switch (sr_type) {
    case SR_CHANNEL_LOGIC: return 0;
    case SR_CHANNEL_DSO: return 1;
    case SR_CHANNEL_ANALOG: return 2;
    }
    return 3;
}

static int rs_init(struct sr_output *o, GHashTable *options, const char *format)
{
    struct rs_context *ctx;
    struct dsv_export_channel *chans;
    GVariant *type_v;
    GSList *l;
    int type, n = 0, i = 0;

    if (!o || !o->sdi)
        return SR_ERR_ARG;

    type_v = options ? g_hash_table_lookup(options, "type") : NULL;
    type = type_v ? g_variant_get_int16(type_v) : SR_CHANNEL_LOGIC;

    ctx = g_try_malloc0(sizeof(*ctx));
    if (ctx == NULL)
        return SR_ERR_MALLOC;

    n = g_slist_length(o->sdi->channels);
    chans = g_try_malloc0(sizeof(*chans) * (n > 0 ? n : 1));
    if (chans == NULL) {
        g_free(ctx);
        return SR_ERR_MALLOC;
    }
    for (l = o->sdi->channels; l; l = l->next, i++) {
        const struct sr_channel *ch = l->data;
        chans[i].index = ch->index;
        chans[i].kind = kind_code(ch->type);
        chans[i].enabled = ch->enabled;
        chans[i].name = ch->name;
        chans[i].vdiv = ch->vdiv;
        chans[i].vfactor = ch->vfactor;
        chans[i].hw_offset = ch->hw_offset;
        chans[i].map_max = ch->map_max;
        chans[i].map_min = ch->map_min;
        chans[i].map_unit = ch->map_unit;
        if (ch->enabled && ch->type == type)
            ctx->enabled_of_type++;
    }
    ctx->all_channels = n;

    if (dsv_export_new(format, kind_code(type), o->start_sample_index, chans, n, &ctx->exporter) != DSV_OK) {
        sr_err("%s", dsv_last_error());
        g_free(chans);
        g_free(ctx);
        return SR_ERR;
    }
    g_free(chans);
    o->priv = ctx;
    return SR_OK;
}

static int csv_init(struct sr_output *o, GHashTable *options)
{
    return rs_init(o, options, "csv");
}

static int vcd_init(struct sr_output *o, GHashTable *options)
{
    return rs_init(o, options, "vcd");
}

static int rs_receive(const struct sr_output *o, const struct sr_datafeed_packet *packet, GString **out)
{
    struct rs_context *ctx;
    struct dsv_export_packet p;
    char *text = NULL;
    GSList *l;

    *out = NULL;
    if (!o || !(ctx = o->priv))
        return SR_ERR_ARG;

    /* StoreSession fills time_string after init; VCD's $date is the current time (as vcd.c). */
    if (!ctx->strings_set) {
        time_t t = time(NULL);
        char *date = g_strdup(ctime(&t));
        date[strlen(date) - 1] = '\0';
        dsv_export_set_strings(ctx->exporter, o->time_string, date);
        g_free(date);
        ctx->strings_set = 1;
    }

    memset(&p, 0, sizeof(p));
    switch (packet->type) {
    case SR_DF_META: {
        const struct sr_datafeed_meta *meta = packet->payload;
        p.kind = DSV_EXPORT_META;
        for (l = meta->config; l; l = l->next) {
            const struct sr_config *src = l->data;
            if (src->key == SR_CONF_SAMPLERATE) {
                p.has_samplerate = 1;
                p.samplerate = g_variant_get_uint64(src->data);
            } else if (src->key == SR_CONF_LIMIT_SAMPLES) {
                p.has_limit_samples = 1;
                p.limit_samples = g_variant_get_uint64(src->data);
            } else if (src->key == SR_CONF_REF_MIN) {
                p.has_ref_min = 1;
                p.ref_min = g_variant_get_uint32(src->data);
            } else if (src->key == SR_CONF_REF_MAX) {
                p.has_ref_max = 1;
                p.ref_max = g_variant_get_uint32(src->data);
            }
        }
        break;
    }
    case SR_DF_LOGIC: {
        const struct sr_datafeed_logic *logic = packet->payload;
        p.kind = DSV_EXPORT_LOGIC;
        p.data = logic->data;
        p.len = logic->length;
        p.unitsize = logic->unitsize;
        p.export_original = packet->bExportOriginalData;
        break;
    }
    case SR_DF_DSO: {
        const struct sr_datafeed_dso *dso = packet->payload;
        p.kind = DSV_EXPORT_DSO;
        p.data = dso->data;
        p.num_samples = dso->num_samples;
        p.len = (uint64_t)dso->num_samples * ctx->enabled_of_type;
        break;
    }
    case SR_DF_ANALOG: {
        const struct sr_datafeed_analog *analog = packet->payload;
        p.kind = DSV_EXPORT_ANALOG;
        p.data = analog->data;
        p.num_samples = analog->num_samples;
        p.len = (uint64_t)analog->num_samples * ctx->all_channels;
        break;
    }
    case SR_DF_END:
        p.kind = DSV_EXPORT_END;
        break;
    default:
        return SR_OK;
    }

    if (dsv_export_receive(ctx->exporter, &p, &text) != DSV_OK) {
        sr_err("%s", dsv_last_error());
        return SR_ERR;
    }
    if (text) {
        *out = g_string_new(text);
        dsv_string_free(text);
    }
    return SR_OK;
}

static int rs_cleanup(struct sr_output *o)
{
    struct rs_context *ctx;

    if (!o || !(ctx = o->priv))
        return SR_ERR_ARG;
    dsv_export_free(ctx->exporter);
    g_free(ctx);
    o->priv = NULL;
    return SR_OK;
}

SR_PRIV struct sr_output_module output_csv_rs = {
    .id = "csv",
    .name = "CSV",
    .desc = "Comma-separated values",
    .exts = (const char *[]){"csv", NULL},
    .options = NULL,
    .init = csv_init,
    .receive = rs_receive,
    .cleanup = rs_cleanup,
};

SR_PRIV struct sr_output_module output_vcd_rs = {
    .id = "vcd",
    .name = "VCD",
    .desc = "Value Change Dump",
    .exts = (const char *[]){"vcd", NULL},
    .options = NULL,
    .init = vcd_init,
    .receive = rs_receive,
    .cleanup = rs_cleanup,
};

#endif /* DSVIEW_RUST_CORE */
