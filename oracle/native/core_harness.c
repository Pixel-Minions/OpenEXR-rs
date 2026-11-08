/*
** SPDX-License-Identifier: BSD-3-Clause
** Copyright Contributors to the OpenEXR Project.
*/

/*
 * The source-built Core harness (PLAN.md O1, owner 2026-11-07): OpenEXRCore 3.5.3 itself,
 * built from the pinned submodule with the wheel's settings (oracle/native/CMakeLists.txt),
 * driven through its public C API only. It reports what Core does and computes nothing.
 * The wheel stays the authority: the harness is used only for what the wheel can't reach,
 * and only where it reproduces the wheel (docs/baseline.md).
 *
 *   core_harness info <file> [--strict] [--silent] [--verbose] [--no-print] [--read]
 *       exr_start_read (with EXR_CONTEXT_FLAG_STRICT_HEADER or _SILENT_HEADER_PARSE), then
 *       exr_print_context_info (unless --no-print), then with --read every chunk decoded
 *       without outputs, in chunk order, one result line each. Core's messages go to stderr
 *       through its default error handler, exactly as the library prints them; the rest to
 *       stdout; the last line of stdout is "result <code name>".
 *
 *   core_harness recompress <in> <out> <compression> [--zip N] [--zstd N] [--dwa F]
 *       Reads <in> with Core and writes <out> with Core alone: each part added with its name
 *       and storage, its attributes copied (exr_copy_unset_attributes), its compression (and
 *       level) set, the header written, then its pixels decoded into planar buffers of the
 *       channels' own types (a scanline part whole, a tiled part tile by tile) and its
 *       chunks encoded from them in chunk order. Deep parts are refused.
 */

#include <openexr.h>

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int
fail (const char* what, exr_result_t rv)
{
    fprintf (stderr, "core_harness: %s: %s\n", what, exr_get_error_code_as_string (rv));
    fflush (stderr);
    return 2;
}

#define CHECK(call)                                                            \
    do                                                                         \
    {                                                                          \
        exr_result_t check_rv_ = (call);                                       \
        if (check_rv_ != EXR_ERR_SUCCESS) return fail (#call, check_rv_);      \
    } while (0)

/* Decodes one chunk without outputs (decompression only); its result on stdout, and when it
 * decoded, the FNV-1a 64 digest of its unpacked bytes (the chunk's unpacked size of them):
 * a fingerprint of what Core produced, not a value computed in its place. */
static void
read_chunk (exr_const_context_t f, int p, const exr_chunk_info_t* ci)
{
    exr_decode_pipeline_t dec = EXR_DECODE_PIPELINE_INITIALIZER;
    exr_result_t          rv  = exr_decoding_initialize (f, p, ci, &dec);
    uint64_t              h   = 14695981039346656037ULL;
    if (rv == EXR_ERR_SUCCESS) rv = exr_decoding_choose_default_routines (f, p, &dec);
    if (rv == EXR_ERR_SUCCESS) rv = exr_decoding_run (f, p, &dec);
    if (rv == EXR_ERR_SUCCESS && dec.unpacked_buffer)
    {
        const uint8_t* b = (const uint8_t*) dec.unpacked_buffer;
        for (uint64_t i = 0; i < ci->unpacked_size; ++i)
        {
            h ^= b[i];
            h *= 1099511628211ULL;
        }
        printf ("chunk %d %d %s %016llx\n", p, ci->idx, exr_get_error_code_as_string (rv),
                (unsigned long long) h);
    }
    else
        printf ("chunk %d %d %s\n", p, ci->idx, exr_get_error_code_as_string (rv));
    exr_decoding_destroy (f, &dec);
}

static void
read_tiles (exr_const_context_t f, int p, int lx, int ly)
{
    int32_t cx, cy;
    if (exr_get_tile_counts (f, p, lx, ly, &cx, &cy) != EXR_ERR_SUCCESS) return;
    for (int ty = 0; ty < cy; ++ty)
        for (int tx = 0; tx < cx; ++tx)
        {
            exr_chunk_info_t ci;
            exr_result_t     rv = exr_read_tile_chunk_info (f, p, tx, ty, lx, ly, &ci);
            if (rv == EXR_ERR_SUCCESS)
                read_chunk (f, p, &ci);
            else
                printf ("tile %d %d %d %d %d %s\n", p, tx, ty, lx, ly, exr_get_error_code_as_string (rv));
        }
}

/* Every chunk of every part, in chunk order, each decoded whatever the others gave. */
static void
read_all (exr_const_context_t f)
{
    int np = 0;
    exr_get_count (f, &np);
    for (int p = 0; p < np; ++p)
    {
        exr_storage_t st;
        if (exr_get_storage (f, p, &st) != EXR_ERR_SUCCESS) continue;
        if (st == EXR_STORAGE_SCANLINE || st == EXR_STORAGE_DEEP_SCANLINE)
        {
            exr_attr_box2i_t dw;
            int32_t          lpc;
            if (exr_get_data_window (f, p, &dw) != EXR_ERR_SUCCESS) continue;
            if (exr_get_scanlines_per_chunk (f, p, &lpc) != EXR_ERR_SUCCESS) continue;
            for (int64_t y = dw.min.y; y <= dw.max.y; y += lpc)
            {
                exr_chunk_info_t ci;
                exr_result_t     rv = exr_read_scanline_chunk_info (f, p, (int) y, &ci);
                if (rv == EXR_ERR_SUCCESS)
                    read_chunk (f, p, &ci);
                else
                    printf ("line %d %d %s\n", p, (int) y, exr_get_error_code_as_string (rv));
            }
        }
        else
        {
            int32_t               levelsx, levelsy;
            uint32_t              tw, th;
            exr_tile_level_mode_t mode;
            exr_tile_round_mode_t round;
            if (exr_get_tile_descriptor (f, p, &tw, &th, &mode, &round) != EXR_ERR_SUCCESS) continue;
            if (exr_get_tile_levels (f, p, &levelsx, &levelsy) != EXR_ERR_SUCCESS) continue;
            if (mode == EXR_TILE_RIPMAP_LEVELS)
            {
                for (int ly = 0; ly < levelsy; ++ly)
                    for (int lx = 0; lx < levelsx; ++lx)
                        read_tiles (f, p, lx, ly);
            }
            else
            {
                for (int l = 0; l < levelsx; ++l)
                    read_tiles (f, p, l, l);
            }
        }
    }
}

static int
cmd_info (int argc, char** argv)
{
    exr_context_t             f     = NULL;
    exr_context_initializer_t cinit = EXR_DEFAULT_CONTEXT_INITIALIZER;
    int                       verbose = 0, print = 1, read = 0;
    exr_result_t              rv;

    if (argc < 1) return fail ("info: missing file", EXR_ERR_INVALID_ARGUMENT);
    for (int i = 1; i < argc; ++i)
    {
        if (!strcmp (argv[i], "--strict"))
            cinit.flags |= EXR_CONTEXT_FLAG_STRICT_HEADER;
        else if (!strcmp (argv[i], "--verbose"))
            verbose = 1;
        else if (!strcmp (argv[i], "--silent"))
            cinit.flags |= EXR_CONTEXT_FLAG_SILENT_HEADER_PARSE;
        else if (!strcmp (argv[i], "--read"))
            read = 1;
        else if (!strcmp (argv[i], "--no-print"))
            print = 0;
        else
            return fail ("info: unknown option", EXR_ERR_INVALID_ARGUMENT);
    }

    rv = exr_start_read (&f, argv[0], &cinit);
    fflush (stderr);
    if (rv == EXR_ERR_SUCCESS && print)
    {
        rv = exr_print_context_info (f, verbose);
        fflush (stdout);
        fflush (stderr);
    }
    if (rv == EXR_ERR_SUCCESS && read)
    {
        read_all (f);
        fflush (stdout);
        fflush (stderr);
    }
    exr_finish (&f);
    printf ("result %s\n", exr_get_error_code_as_string (rv));
    fflush (stdout);
    return 0;
}

/* A chunk's channels as planar buffers of their own types. */
typedef struct
{
    uint8_t* data;
    size_t   offsets[256];
} planes_t;

static int
plan_channels (
    exr_coding_channel_info_t* chans, int16_t count, planes_t* planes)
{
    size_t total = 0;
    if (count > 256) return fail ("too many channels", EXR_ERR_INVALID_ARGUMENT);
    for (int16_t c = 0; c < count; ++c)
    {
        planes->offsets[c] = total;
        total += (size_t) chans[c].width * (size_t) chans[c].height *
                 (size_t) chans[c].bytes_per_element;
    }
    planes->data = (uint8_t*) malloc (total ? total : 1);
    if (!planes->data) return fail ("out of memory", EXR_ERR_OUT_OF_MEMORY);
    memset (planes->data, 0, total ? total : 1);
    return 0;
}

static int
copy_chunk (
    exr_const_context_t      in,
    exr_context_t            out,
    int                      p,
    const exr_chunk_info_t*  cin,
    const exr_chunk_info_t*  cout)
{
    exr_decode_pipeline_t dec = EXR_DECODE_PIPELINE_INITIALIZER;
    exr_encode_pipeline_t enc = EXR_ENCODE_PIPELINE_INITIALIZER;
    planes_t              planes;
    int                   r;

    CHECK (exr_decoding_initialize (in, p, cin, &dec));
    r = plan_channels (dec.channels, dec.channel_count, &planes);
    if (r) return r;
    for (int16_t c = 0; c < dec.channel_count; ++c)
    {
        exr_coding_channel_info_t* ch = dec.channels + c;
        ch->decode_to_ptr          = planes.data + planes.offsets[c];
        ch->user_pixel_stride      = ch->bytes_per_element;
        ch->user_line_stride       = ch->width * ch->bytes_per_element;
        ch->user_bytes_per_element = ch->bytes_per_element;
        ch->user_data_type         = ch->data_type;
    }
    CHECK (exr_decoding_choose_default_routines (in, p, &dec));
    CHECK (exr_decoding_run (in, p, &dec));

    CHECK (exr_encoding_initialize (out, p, cout, &enc));
    if (enc.channel_count != dec.channel_count)
        return fail ("channel counts differ", EXR_ERR_INVALID_ARGUMENT);
    /* The encoder's own planes, of the sizes the write side's chunk info gives: they differ
     * from the read side's where Core's exr_write_tile_chunk_info keeps a whole tile at a
     * level's edge (docs/improvements.md, I-21). The decoded rows are copied in, the rest is
     * zero, so that Core reads only memory it is given. */
    {
        planes_t eplanes;
        r = plan_channels (enc.channels, enc.channel_count, &eplanes);
        if (r) return r;
        for (int16_t c = 0; c < enc.channel_count; ++c)
        {
            const exr_coding_channel_info_t* d = dec.channels + c;
            const exr_coding_channel_info_t* e = enc.channels + c;
            int32_t rows = d->height < e->height ? d->height : e->height;
            int32_t cols = d->width < e->width ? d->width : e->width;
            for (int32_t y = 0; y < rows; ++y)
                memcpy (
                    eplanes.data + eplanes.offsets[c] +
                        (size_t) y * (size_t) e->width * e->bytes_per_element,
                    planes.data + planes.offsets[c] +
                        (size_t) y * (size_t) d->width * d->bytes_per_element,
                    (size_t) cols * d->bytes_per_element);
        }
        free (planes.data);
        planes = eplanes;
    }
    for (int16_t c = 0; c < enc.channel_count; ++c)
    {
        exr_coding_channel_info_t* ch = enc.channels + c;
        ch->encode_from_ptr        = planes.data + planes.offsets[c];
        ch->user_pixel_stride      = ch->bytes_per_element;
        ch->user_line_stride       = ch->width * ch->bytes_per_element;
        ch->user_bytes_per_element = ch->bytes_per_element;
        ch->user_data_type         = ch->data_type;
    }
    CHECK (exr_encoding_choose_default_routines (out, p, &enc));
    CHECK (exr_encoding_run (out, p, &enc));
    CHECK (exr_encoding_destroy (out, &enc));
    CHECK (exr_decoding_destroy (in, &dec));
    free (planes.data);
    return 0;
}

/* floor (x / s) */
static int64_t
floor_div (int64_t x, int64_t s)
{
    return x >= 0 ? x / s : -((-x + s - 1) / s);
}

/* how many of the lines (or columns) [a, b) a channel sampled every s holds */
static int64_t
sampled (int64_t a, int64_t b, int64_t s)
{
    return floor_div (b - 1, s) - floor_div (a - 1, s);
}

/* A scanline part: its chunk height follows its compression, so the whole part is decoded
 * into planar buffers of the channels' own types first, then the output's chunks are encoded
 * from them. */
static int
copy_scanlines (exr_const_context_t in, exr_context_t out, int p)
{
    exr_attr_box2i_t          dw;
    int32_t                   lpc_in, lpc_out;
    const exr_attr_chlist_t*  chans = NULL;
    uint8_t*                  planes[256];
    int64_t                   widths[256];
    int                       bpe[256];

    CHECK (exr_get_data_window (in, p, &dw));
    CHECK (exr_get_scanlines_per_chunk (in, p, &lpc_in));
    CHECK (exr_get_scanlines_per_chunk (out, p, &lpc_out));
    CHECK (exr_get_channels (in, p, &chans));
    if (chans->num_channels > 256) return fail ("too many channels", EXR_ERR_INVALID_ARGUMENT);
    for (int c = 0; c < chans->num_channels; ++c)
    {
        const exr_attr_chlist_entry_t* e = chans->entries + c;
        int64_t h = sampled (dw.min.y, (int64_t) dw.max.y + 1, e->y_sampling);
        widths[c] = sampled (dw.min.x, (int64_t) dw.max.x + 1, e->x_sampling);
        bpe[c]    = e->pixel_type == EXR_PIXEL_HALF ? 2 : 4;
        planes[c] = (uint8_t*) calloc ((size_t) (widths[c] * h * bpe[c]) + 1, 1);
        if (!planes[c]) return fail ("out of memory", EXR_ERR_OUT_OF_MEMORY);
    }

    for (int64_t y = dw.min.y; y <= dw.max.y; y += lpc_in)
    {
        exr_chunk_info_t      cin;
        exr_decode_pipeline_t dec = EXR_DECODE_PIPELINE_INITIALIZER;
        CHECK (exr_read_scanline_chunk_info (in, p, (int) y, &cin));
        CHECK (exr_decoding_initialize (in, p, &cin, &dec));
        for (int16_t c = 0; c < dec.channel_count; ++c)
        {
            exr_coding_channel_info_t* ch = dec.channels + c;
            const exr_attr_chlist_entry_t* e = chans->entries + c;
            int64_t row = sampled (dw.min.y, cin.start_y, e->y_sampling);
            if (ch->height > 0 && ch->width != widths[c])
                return fail ("a chunk's channel width differs", EXR_ERR_INVALID_ARGUMENT);
            ch->decode_to_ptr          = planes[c] + row * widths[c] * bpe[c];
            ch->user_pixel_stride      = ch->bytes_per_element;
            ch->user_line_stride       = (int32_t) (widths[c] * bpe[c]);
            ch->user_bytes_per_element = ch->bytes_per_element;
            ch->user_data_type         = ch->data_type;
        }
        CHECK (exr_decoding_choose_default_routines (in, p, &dec));
        CHECK (exr_decoding_run (in, p, &dec));
        CHECK (exr_decoding_destroy (in, &dec));
    }

    for (int64_t y = dw.min.y; y <= dw.max.y; y += lpc_out)
    {
        exr_chunk_info_t      cout;
        exr_encode_pipeline_t enc = EXR_ENCODE_PIPELINE_INITIALIZER;
        CHECK (exr_write_scanline_chunk_info (out, p, (int) y, &cout));
        CHECK (exr_encoding_initialize (out, p, &cout, &enc));
        for (int16_t c = 0; c < enc.channel_count; ++c)
        {
            exr_coding_channel_info_t* ch = enc.channels + c;
            const exr_attr_chlist_entry_t* e = chans->entries + c;
            int64_t row = sampled (dw.min.y, cout.start_y, e->y_sampling);
            ch->encode_from_ptr        = planes[c] + row * widths[c] * bpe[c];
            ch->user_pixel_stride      = ch->bytes_per_element;
            ch->user_line_stride       = (int32_t) (widths[c] * bpe[c]);
            ch->user_bytes_per_element = ch->bytes_per_element;
            ch->user_data_type         = ch->data_type;
        }
        CHECK (exr_encoding_choose_default_routines (out, p, &enc));
        CHECK (exr_encoding_run (out, p, &enc));
        CHECK (exr_encoding_destroy (out, &enc));
    }

    for (int c = 0; c < chans->num_channels; ++c)
        free (planes[c]);
    return 0;
}

static int
copy_tiles (exr_const_context_t in, exr_context_t out, int p, int lx, int ly)
{
    int32_t cx, cy;
    CHECK (exr_get_tile_counts (in, p, lx, ly, &cx, &cy));
    for (int ty = 0; ty < cy; ++ty)
    {
        for (int tx = 0; tx < cx; ++tx)
        {
            exr_chunk_info_t cin, cout;
            int              r;
            CHECK (exr_read_tile_chunk_info (in, p, tx, ty, lx, ly, &cin));
            CHECK (exr_write_tile_chunk_info (out, p, tx, ty, lx, ly, &cout));
            r = copy_chunk (in, out, p, &cin, &cout);
            if (r) return r;
        }
    }
    return 0;
}

static int
cmd_recompress (int argc, char** argv)
{
    exr_context_t             in = NULL, out = NULL;
    exr_context_initializer_t cinit = EXR_DEFAULT_CONTEXT_INITIALIZER;
    exr_compression_t         comp;
    int                       zip = 0, zstd = 0, have_zip = 0, have_zstd = 0, have_dwa = 0, nparts = 0;
    float                     dwa = 0.f;

    if (argc < 3) return fail ("recompress: <in> <out> <compression>", EXR_ERR_INVALID_ARGUMENT);
    comp = (exr_compression_t) atoi (argv[2]);
    for (int i = 3; i + 1 < argc; i += 2)
    {
        if (!strcmp (argv[i], "--zip"))
            { zip = atoi (argv[i + 1]); have_zip = 1; }
        else if (!strcmp (argv[i], "--zstd"))
            { zstd = atoi (argv[i + 1]); have_zstd = 1; }
        else if (!strcmp (argv[i], "--dwa"))
        {
            dwa      = (float) atof (argv[i + 1]);
            have_dwa = 1;
        }
        else
            return fail ("recompress: unknown option", EXR_ERR_INVALID_ARGUMENT);
    }

    CHECK (exr_start_read (&in, argv[0], &cinit));
    CHECK (exr_start_write (&out, argv[1], EXR_WRITE_FILE_DIRECTLY, &cinit));
    CHECK (exr_get_count (in, &nparts));
    for (int p = 0; p < nparts; ++p)
    {
        const char*   name = NULL;
        exr_storage_t st;
        int           idx = -1;
        {
            /* a single-part file may have no name */
            exr_result_t nrv = exr_get_name (in, p, &name);
            if (nrv == EXR_ERR_NO_ATTR_BY_NAME)
                name = NULL;
            else if (nrv != EXR_ERR_SUCCESS)
                return fail ("exr_get_name", nrv);
        }
        CHECK (exr_get_storage (in, p, &st));
        if (st == EXR_STORAGE_DEEP_SCANLINE || st == EXR_STORAGE_DEEP_TILED)
            return fail ("recompress: deep parts are not supported", EXR_ERR_INVALID_ARGUMENT);
        CHECK (exr_add_part (out, name, st, &idx));
        CHECK (exr_copy_unset_attributes (out, idx, in, p));
        CHECK (exr_set_compression (out, idx, comp));
        if (have_zip) CHECK (exr_set_zip_compression_level (out, idx, zip));
        if (have_zstd) CHECK (exr_set_zstd_compression_level (out, idx, zstd));
        if (have_dwa) CHECK (exr_set_dwa_compression_level (out, idx, dwa));
    }
    CHECK (exr_write_header (out));

    for (int p = 0; p < nparts; ++p)
    {
        exr_storage_t st;
        int           r;
        CHECK (exr_get_storage (in, p, &st));
        if (st == EXR_STORAGE_SCANLINE)
        {
            if ((r = copy_scanlines (in, out, p))) return r;
        }
        else
        {
            int32_t              levelsx, levelsy;
            uint32_t             tw, th;
            exr_tile_level_mode_t mode;
            exr_tile_round_mode_t round;
            CHECK (exr_get_tile_descriptor (in, p, &tw, &th, &mode, &round));
            CHECK (exr_get_tile_levels (in, p, &levelsx, &levelsy));
            if (mode == EXR_TILE_RIPMAP_LEVELS)
            {
                for (int ly = 0; ly < levelsy; ++ly)
                    for (int lx = 0; lx < levelsx; ++lx)
                        if ((r = copy_tiles (in, out, p, lx, ly))) return r;
            }
            else
            {
                for (int l = 0; l < levelsx; ++l)
                    if ((r = copy_tiles (in, out, p, l, l))) return r;
            }
        }
    }

    CHECK (exr_finish (&out));
    CHECK (exr_finish (&in));
    return 0;
}

/* exr_compress_buffer of a whole file's bytes at a level, into another file. */
static int
cmd_compress_buffer (int argc, char** argv)
{
    exr_context_t f = NULL;
    FILE*         fp;
    long          n;
    uint8_t *     in, *out;
    size_t        cap, actual = 0;
    if (argc != 3) return fail ("compress-buffer: <level> <in> <out>", EXR_ERR_INVALID_ARGUMENT);
    fp = fopen (argv[1], "rb");
    if (!fp) return fail ("compress-buffer: cannot open the input", EXR_ERR_FILE_ACCESS);
    fseek (fp, 0, SEEK_END);
    n = ftell (fp);
    fseek (fp, 0, SEEK_SET);
    in = (uint8_t*) malloc ((size_t) n + 1);
    if (fread (in, 1, (size_t) n, fp) != (size_t) n) return fail ("short read", EXR_ERR_READ_IO);
    fclose (fp);
    cap = exr_compress_max_buffer_size ((size_t) n);
    out = (uint8_t*) malloc (cap);
    CHECK (exr_start_temporary_context (&f, "<compress-buffer>", NULL));
    CHECK (exr_compress_buffer (f, atoi (argv[0]), in, (size_t) n, out, cap, &actual));
    exr_finish (&f);
    fp = fopen (argv[2], "wb");
    if (!fp) return fail ("compress-buffer: cannot open the output", EXR_ERR_FILE_ACCESS);
    fwrite (out, 1, actual, fp);
    fclose (fp);
    free (in);
    free (out);
    return 0;
}

int
main (int argc, char** argv)
{
    if (argc >= 2 && !strcmp (argv[1], "compress-buffer"))
        return cmd_compress_buffer (argc - 2, argv + 2);
    if (argc >= 2 && !strcmp (argv[1], "info")) return cmd_info (argc - 2, argv + 2);
    if (argc >= 2 && !strcmp (argv[1], "recompress"))
        return cmd_recompress (argc - 2, argv + 2);
    fprintf (
        stderr,
        "usage: core_harness info <file> [--strict] [--verbose]\n"
        "       core_harness recompress <in> <out> <compression> [--zip N] [--zstd N] [--dwa F]\n");
    return 2;
}
