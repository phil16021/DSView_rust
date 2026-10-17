/*
 * Export oracle: drives libsigrok4DSL's output modules exactly like
 * StoreSession::export_exec (init with "filename"/"type", META, data
 * packets, END) and returns the concatenated text.
 */

#include <glib.h>
#include <string.h>
#include "libsigrok.h"
#include "libsigrok-internal.h"

#include "oracle.h"

void oracle_free(void *p)
{
    g_free(p);
}

static int channel_type(int kind)
{
    switch (kind) {
    case ORACLE_CH_LOGIC: return SR_CHANNEL_LOGIC;
    case ORACLE_CH_DSO: return SR_CHANNEL_DSO;
    case ORACLE_CH_ANALOG: return SR_CHANNEL_ANALOG;
    }
    return SR_CHANNEL_GROUP;
}

int oracle_export_run(const char *module_id, int kind, uint64_t start_index, const char *time_string,
                      const struct oracle_export_channel *chans, int n_ch,
                      const struct oracle_export_packet *pkts, int n_pkts, char **out)
{
    const struct sr_output_module **mods = sr_output_list();
    const struct sr_output_module *mod = NULL;
    struct sr_dev_inst sdi;
    struct sr_output o;
    GString *text = g_string_new(NULL);
    GHashTable *params;
    int ret = 0;

    for (; mods && *mods; mods++) {
        if (strcmp((*mods)->id, module_id) == 0)
            mod = *mods;
    }
    if (mod == NULL)
        return -1;

    memset(&sdi, 0, sizeof(sdi));
    for (int i = 0; i < n_ch; i++) {
        struct sr_channel *ch = sr_channel_new(chans[i].index, channel_type(chans[i].kind), chans[i].enabled, chans[i].name);
        ch->vdiv = chans[i].vdiv;
        ch->vfactor = chans[i].vfactor;
        ch->hw_offset = chans[i].hw_offset;
        ch->map_max = chans[i].map_max;
        ch->map_min = chans[i].map_min;
        ch->map_unit = chans[i].map_unit;
        sdi.channels = g_slist_append(sdi.channels, ch);
    }

    params = g_hash_table_new(g_str_hash, g_str_equal);
    GVariant *fname = g_variant_ref_sink(g_variant_new_bytestring("oracle-export"));
    GVariant *type = g_variant_ref_sink(g_variant_new_int16(channel_type(kind)));
    g_hash_table_insert(params, (char *)"filename", fname);
    g_hash_table_insert(params, (char *)"type", type);

    memset(&o, 0, sizeof(o));
    o.module = mod;
    o.sdi = &sdi;
    o.start_sample_index = start_index;

    if (mod->init && mod->init(&o, params) != SR_OK) {
        ret = -1;
        goto done;
    }
    g_strlcpy(o.time_string, time_string, sizeof(o.time_string));

    for (int i = 0; i < n_pkts; i++) {
        const struct oracle_export_packet *pk = &pkts[i];
        struct sr_datafeed_packet p;
        struct sr_datafeed_meta meta;
        struct sr_datafeed_logic logic;
        struct sr_datafeed_dso dso;
        struct sr_datafeed_analog analog;
        GString *chunk = NULL;

        memset(&p, 0, sizeof(p));
        memset(&meta, 0, sizeof(meta));
        memset(&logic, 0, sizeof(logic));
        memset(&dso, 0, sizeof(dso));
        memset(&analog, 0, sizeof(analog));
        p.status = SR_PKT_OK;
        p.bExportOriginalData = pk->export_original;

        switch (pk->type) {
        case ORACLE_EXP_META:
            meta.config = g_slist_append(NULL, ds_new_config(SR_CONF_SAMPLERATE, g_variant_new_uint64(pk->samplerate)));
            meta.config = g_slist_append(meta.config, ds_new_config(SR_CONF_LIMIT_SAMPLES, g_variant_new_uint64(pk->limit_samples)));
            if (pk->has_ref) {
                meta.config = g_slist_append(meta.config, ds_new_config(SR_CONF_REF_MIN, g_variant_new_uint32(pk->ref_min)));
                meta.config = g_slist_append(meta.config, ds_new_config(SR_CONF_REF_MAX, g_variant_new_uint32(pk->ref_max)));
            }
            p.type = SR_DF_META;
            p.payload = &meta;
            break;
        case ORACLE_EXP_LOGIC:
            logic.data = (void *)pk->data;
            logic.length = pk->len;
            logic.unitsize = pk->unitsize;
            p.type = SR_DF_LOGIC;
            p.payload = &logic;
            break;
        case ORACLE_EXP_DSO:
            dso.data = (void *)pk->data;
            dso.num_samples = pk->num_samples;
            p.type = SR_DF_DSO;
            p.payload = &dso;
            break;
        case ORACLE_EXP_ANALOG:
            analog.data = (void *)pk->data;
            analog.num_samples = pk->num_samples;
            p.type = SR_DF_ANALOG;
            p.payload = &analog;
            break;
        default:
            p.type = SR_DF_END;
            break;
        }

        mod->receive(&o, &p, &chunk);
        if (chunk) {
            g_string_append_len(text, chunk->str, chunk->len);
            g_string_free(chunk, TRUE);
        }
        for (GSList *l = meta.config; l; l = l->next)
            ds_free_config(l->data);
        g_slist_free(meta.config);
    }

    if (mod->cleanup)
        mod->cleanup(&o);

done:
    g_hash_table_destroy(params);
    g_variant_unref(fname);
    g_variant_unref(type);
    /* map_unit is borrowed from the caller; sr_dev_probes_free leaves it alone. */
    sr_dev_probes_free(&sdi);
    *out = g_string_free(text, FALSE);
    return ret;
}
