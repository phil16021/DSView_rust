/*
 * DSVIEW_IMPL_DECODE=rust: the public libsigrokdecode API backed by the Rust
 * decoder engine (crates/dsview-decode via dsv_decode_* in dsview_ffi.h).
 *
 * The C structs the Qt GUI reads (struct srd_decoder lists, struct
 * srd_session di_list, struct srd_decoder_inst channel maps, struct
 * srd_proto_data in callbacks) are kept and filled from the Rust engine's
 * answers; Python is owned by the Rust engine.
 */

#ifdef DSVIEW_RUST_CORE

#include "config.h"
#include "libsigrokdecode-internal.h"
#include "libsigrokdecode.h"
#include <string.h>
#include <wchar.h>
#include <stdlib.h>
#include "log.h"
#include "dsview_ffi.h"

extern SRD_PRIV GSList *sessions;
extern SRD_PRIV int max_session_id;
extern SRD_PRIV GSList *searchpaths;

SRD_PRIV int srd_rs_enabled(void)
{
	static int on = -1;
	if (on < 0)
		on = dsv_use_rust("DECODE") ? 1 : 0;
	return on;
}

static void rs_log(int32_t level, const char *msg)
{
	switch (level) {
	case 1: srd_err("%s", msg); break;
	case 2: srd_warn("%s", msg); break;
	case 3: srd_info("%s", msg); break;
	case 4: srd_dbg("%s", msg); break;
	default: srd_detail("%s", msg); break;
	}
}

/* ------------------------------------------------------------ init */

SRD_PRIV int srd_rs_init(const char *path)
{
	const char *env;

	dsv_decode_set_log(rs_log);
	if (!path)
		path = g_getenv("SIGROKDECODE_DIR");
	if (dsv_decode_init(path) != DSV_OK) {
		srd_err("Rust decoder engine: %s", dsv_last_error());
		return SRD_ERR_PYTHON;
	}
	if (path)
		searchpaths = g_slist_prepend(searchpaths, g_strdup(path));
	(void)env;
	max_session_id = 0;
	return SRD_OK;
}

SRD_PRIV void srd_rs_set_python_home(const wchar_t *path)
{
	/* Py_SetPythonHome is gone from newer Pythons; the environment works
	 * as long as it is set before the interpreter starts. */
	char buf[4096];
	if (wcstombs(buf, path, sizeof(buf)) < sizeof(buf))
		g_setenv("PYTHONHOME", buf, TRUE);
}

/* ------------------------------------------------------------ decoders */

static GVariant *rs_variant(const struct dsv_value *v)
{
	switch (v->kind) {
	case 1: return g_variant_ref_sink(g_variant_new_string(v->s ? v->s : ""));
	case 2: return g_variant_ref_sink(g_variant_new_int64(v->i));
	case 3: return g_variant_ref_sink(g_variant_new_double(v->d));
	default: return NULL;
	}
}

static GSList *rs_strlist(const struct dsv_str_list *l)
{
	GSList *out = NULL;
	for (uint32_t i = 0; i < l->n; i++)
		out = g_slist_append(out, g_strdup(l->items[i]));
	return out;
}

static char **rs_strv(const struct dsv_str_list *l)
{
	char **v = g_new0(char *, l->n + 1);
	for (uint32_t i = 0; i < l->n; i++)
		v[i] = g_strdup(l->items[i]);
	return v;
}

static GSList *rs_channels(const struct dsv_channel_meta *c, uint32_t n)
{
	GSList *out = NULL;
	for (uint32_t i = 0; i < n; i++) {
		struct srd_channel *ch = g_new0(struct srd_channel, 1);
		ch->id = g_strdup(c[i].id);
		ch->name = g_strdup(c[i].name);
		ch->desc = g_strdup(c[i].desc);
		ch->order = c[i].order;
		ch->type = c[i].ty;
		ch->idn = c[i].idn ? g_strdup(c[i].idn) : NULL;
		out = g_slist_append(out, ch);
	}
	return out;
}

/* The Rust engine's decoders as `struct srd_decoder`, in the same list
 * orders decoder.c produces (annotations reversed). */
SRD_PRIV int srd_rs_decoder_load_all(GSList **pd_list)
{
	const struct dsv_decoder_meta *m = NULL;
	uint32_t n;

	if (dsv_decode_load_all() != DSV_OK)
		return SRD_ERR;
	n = dsv_decode_list(&m);

	for (uint32_t k = 0; k < n; k++) {
		const struct dsv_decoder_meta *dm = &m[k];
		struct srd_decoder *d = g_new0(struct srd_decoder, 1);

		d->id = g_strdup(dm->id);
		d->name = g_strdup(dm->name);
		d->longname = g_strdup(dm->longname);
		d->desc = g_strdup(dm->desc);
		d->license = g_strdup(dm->license);
		d->inputs = rs_strlist(&dm->inputs);
		d->outputs = rs_strlist(&dm->outputs);
		d->tags = rs_strlist(&dm->tags);
		d->channels = rs_channels(dm->channels, dm->n_channels);
		d->opt_channels = rs_channels(dm->opt_channels, dm->n_opt_channels);

		for (uint32_t i = 0; i < dm->n_options; i++) {
			const struct dsv_option_meta *om = &dm->options[i];
			struct srd_decoder_option *o = g_new0(struct srd_decoder_option, 1);
			o->id = g_strdup(om->id);
			o->idn = om->idn ? g_strdup(om->idn) : NULL;
			o->desc = om->desc ? g_strdup(om->desc) : NULL;
			o->def = rs_variant(&om->def);
			for (uint32_t j = 0; j < om->n_values; j++)
				o->values = g_slist_append(o->values, rs_variant(&om->values[j]));
			d->options = g_slist_append(d->options, o);
		}

		for (uint32_t i = 0; i < dm->n_annotations; i++) {
			d->annotations = g_slist_prepend(d->annotations, rs_strv(&dm->annotations[i]));
			d->ann_types = g_slist_append(d->ann_types, GINT_TO_POINTER(dm->ann_types[i]));
		}

		for (uint32_t i = 0; i < dm->n_rows; i++) {
			struct srd_decoder_annotation_row *r = g_new0(struct srd_decoder_annotation_row, 1);
			r->id = g_strdup(dm->rows[i].id);
			r->desc = g_strdup(dm->rows[i].desc);
			for (uint32_t j = 0; j < dm->rows[i].n_classes; j++)
				r->ann_classes = g_slist_append(r->ann_classes,
						GSIZE_TO_POINTER(dm->rows[i].classes[j]));
			d->annotation_rows = g_slist_append(d->annotation_rows, r);
		}

		for (uint32_t i = 0; i < dm->n_binary; i++)
			d->binary = g_slist_append(d->binary, rs_strv(&dm->binary[i]));

		/* no Python objects on the C side */
		d->py_mod = NULL;
		d->py_dec = NULL;
		*pd_list = g_slist_append(*pd_list, d);
	}
	return SRD_OK;
}

/* ------------------------------------------------------------ outputs */

static struct srd_pd_output *rs_pdo(struct srd_decoder_inst *di, const struct dsv_decode_event *ev)
{
	for (GSList *l = di->pd_output; l; l = l->next) {
		struct srd_pd_output *p = l->data;
		if (p->pdo_id == ev->pdo_id)
			return p;
	}
	struct srd_pd_output *p = g_new0(struct srd_pd_output, 1);
	p->pdo_id = ev->pdo_id;
	p->output_type = ev->output_type;
	p->di = di;
	p->proto_id = g_strdup(ev->proto_id);
	if (ev->meta_float >= 0) {
		p->meta_type = ev->meta_float ? G_VARIANT_TYPE_DOUBLE : G_VARIANT_TYPE_INT64;
		p->meta_name = g_strdup(ev->meta_name);
		p->meta_descr = g_strdup(ev->meta_descr);
	}
	di->pd_output = g_slist_append(di->pd_output, p);
	return p;
}

/* Rust engine output -> srd_proto_data for the frontend callbacks (runs on
 * the stack's decoder thread, as in C). */
static void rs_event(void *user, const struct dsv_decode_event *ev)
{
	struct srd_session *sess = user;
	struct srd_decoder_inst *di = ev->inst_user;
	struct srd_pd_callback *cb;
	struct srd_proto_data pdata;

	if (!di || !(cb = srd_pd_output_callback_find(sess, ev->output_type)))
		return;

	pdata.start_sample = ev->start;
	pdata.end_sample = ev->end;
	pdata.pdo = rs_pdo(di, ev);
	pdata.data = NULL;

	switch (ev->output_type) {
	case SRD_OUTPUT_ANN: {
		struct srd_proto_data_annotation pda;
		memset(&pda, 0, sizeof(pda));
		pda.ann_class = ev->ann_class;
		pda.ann_type = ev->ann_type;
		pda.numberic_value = ev->ann_num;
		if (ev->ann_hex)
			g_strlcpy(pda.str_number_hex, ev->ann_hex, sizeof(pda.str_number_hex));
		if (ev->ann_texts)
			pda.ann_text = g_strdupv((char **)ev->ann_texts);
		pdata.data = &pda;
		cb->cb(&pdata, cb->cb_data);
		g_strfreev(pda.ann_text);
		break;
	}
	case SRD_OUTPUT_BINARY: {
		struct srd_proto_data_binary pdb;
		pdb.bin_class = ev->bin_class;
		pdb.size = ev->bin_len;
		pdb.data = ev->bin_data;
		pdata.data = &pdb;
		cb->cb(&pdata, cb->cb_data);
		break;
	}
	case SRD_OUTPUT_META: {
		GVariant *v = ev->meta_float == 1 ? g_variant_new_double(ev->meta_double)
		                                  : g_variant_new_int64(ev->meta_int);
		g_variant_ref_sink(v);
		pdata.data = v;
		cb->cb(&pdata, cb->cb_data);
		g_variant_unref(v);
		break;
	}
	default:
		break;
	}
}

/* ------------------------------------------------------------ sessions */

SRD_PRIV void srd_rs_session_new(struct srd_session *sess)
{
	sess->rs = dsv_decode_session_new();
	dsv_decode_set_callback(sess->rs, rs_event, sess);
}

SRD_PRIV void srd_rs_session_free(struct srd_session *sess)
{
	dsv_decode_session_free(sess->rs);
	sess->rs = NULL;
}

static void rs_error(char **error)
{
	if (error)
		*error = g_strdup(dsv_last_error());
}

SRD_PRIV int srd_rs_session_start(struct srd_session *sess, char **error)
{
	if (dsv_decode_start(sess->rs) != DSV_OK) {
		rs_error(error);
		return SRD_ERR_PYTHON;
	}
	return SRD_OK;
}

SRD_PRIV int srd_rs_session_send(struct srd_session *sess, uint64_t start, uint64_t end,
		const uint8_t **inbuf, const uint8_t *inbuf_const, uint64_t inbuflen, char **error)
{
	struct srd_decoder_inst *di;
	int ret;

	if (!sess->di_list)
		return SRD_OK;
	if (!inbuf) {
		*error = g_strdup("NULL buffer pointer");
		return SRD_ERR_ARG;
	}
	if (inbuflen == 0) {
		*error = g_strdup("empty buffer");
		return SRD_ERR_ARG;
	}
	di = sess->di_list->data;
	ret = dsv_decode_send(sess->rs, start, end, inbuf, inbuf_const, di->dec_num_channels);
	if (ret == DSV_ERR_ARG)
		return SRD_ERR_ARG;
	if (ret != DSV_OK) {
		rs_error(error);
		return SRD_ERR_TERM_REQ;
	}
	return SRD_OK;
}

SRD_PRIV int srd_rs_session_end(struct srd_session *sess, char **error)
{
	if (dsv_decode_end(sess->rs) != DSV_OK) {
		const char *e = dsv_last_error();
		if (e && e[0])
			rs_error(error);
		return SRD_ERR_PYTHON;
	}
	return SRD_OK;
}

/* ------------------------------------------------------------ instances */

SRD_PRIV struct srd_decoder_inst *srd_rs_inst_new(struct srd_session *sess,
		const char *decoder_id, GHashTable *options)
{
	struct srd_decoder *dec;
	struct srd_decoder_inst *di;
	const dsv_decode_inst *h = NULL;
	const char *inst_id = NULL;
	GHashTableIter it;
	gpointer key, val;
	guint n = 0;
	char idbuf[256];

	if (!(dec = srd_decoder_get_by_id(decoder_id))) {
		srd_err("Protocol decoder %s not found.", decoder_id);
		return NULL;
	}
	if (options) {
		inst_id = g_hash_table_lookup(options, "id");
		n = g_hash_table_size(options);
	}

	const char **keys = g_new0(const char *, n + 1);
	struct dsv_value *vals = g_new0(struct dsv_value, n + 1);
	guint k = 0;
	if (options) {
		g_hash_table_iter_init(&it, options);
		while (g_hash_table_iter_next(&it, &key, &val)) {
			GVariant *v = val;
			if (!strcmp(key, "id") || !v)
				continue;
			keys[k] = key;
			if (g_variant_is_of_type(v, G_VARIANT_TYPE_STRING)) {
				vals[k].kind = 1;
				vals[k].s = g_variant_get_string(v, NULL);
			} else if (g_variant_is_of_type(v, G_VARIANT_TYPE_INT64)) {
				vals[k].kind = 2;
				vals[k].i = g_variant_get_int64(v);
			} else if (g_variant_is_of_type(v, G_VARIANT_TYPE_DOUBLE)) {
				vals[k].kind = 3;
				vals[k].d = g_variant_get_double(v);
			} else {
				continue;
			}
			k++;
		}
	}
	int ret = dsv_decode_inst_new(sess->rs, decoder_id, keys, vals, k, options != NULL,
				      inst_id, &h);
	g_free(keys);
	g_free(vals);
	if (ret != DSV_OK) {
		srd_err("%s", dsv_last_error());
		return NULL;
	}

	di = g_new0(struct srd_decoder_inst, 1);
	di->decoder = dec;
	di->sess = sess;
	dsv_decode_inst_id(h, idbuf, sizeof(idbuf));
	di->inst_id = g_strdup(idbuf);
	di->rs = (void *)h;
	di->dec_num_channels = g_slist_length(dec->channels) + g_slist_length(dec->opt_channels);
	if (di->dec_num_channels > 0) {
		di->dec_channelmap = g_new0(int, di->dec_num_channels);
		dsv_decode_inst_channelmap(h, di->dec_channelmap, di->dec_num_channels);
	}
	dsv_decode_inst_set_user(h, di);
	sess->di_list = g_slist_append(sess->di_list, di);
	return di;
}

SRD_PRIV int srd_rs_inst_channel_set_all(struct srd_decoder_inst *di, GHashTable *new_channels)
{
	GHashTableIter it;
	gpointer key, val;
	guint n = g_hash_table_size(new_channels), k = 0;
	const char **ids = g_new0(const char *, n + 1);
	int32_t *idx = g_new0(int32_t, n + 1);

	g_hash_table_iter_init(&it, new_channels);
	while (g_hash_table_iter_next(&it, &key, &val)) {
		if (!g_variant_is_of_type(val, G_VARIANT_TYPE_INT32)) {
			srd_err("No channel number was specified for %s.", (char *)key);
			g_free(ids);
			g_free(idx);
			return SRD_ERR_ARG;
		}
		ids[k] = key;
		idx[k] = g_variant_get_int32(val);
		k++;
	}
	int ret = dsv_decode_channel_set_all(di->sess->rs, di->rs, ids, idx, k);
	g_free(ids);
	g_free(idx);
	if (ret != DSV_OK)
		return SRD_ERR_ARG;
	if (di->dec_channelmap)
		dsv_decode_inst_channelmap(di->rs, di->dec_channelmap, di->dec_num_channels);
	return SRD_OK;
}

SRD_PRIV void srd_rs_inst_stack(struct srd_session *sess, struct srd_decoder_inst *bottom,
		struct srd_decoder_inst *top)
{
	dsv_decode_stack(sess->rs, bottom->rs, top->rs);
}

#endif /* DSVIEW_RUST_CORE */
