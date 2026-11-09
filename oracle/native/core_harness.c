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
 *
 *   core_harness compress-buffer <level> <in> <out>
 *       exr_compress_buffer of a whole file's bytes at a level.
 *
 *   core_harness convert <dir>
 *       Every half, float and uint conversion of Core's pack and unpack routines, over every
 *       input, through the public pipeline (cmd_convert); digests on stdout.
 *
 *   core_harness bench <in> <reps> [<compression> [--zip N] [--zstd N] [--dwa F]]
 *       Timing for comparing the port's speed with Core's (cmd_bench): part 0's chunks
 *       decoded, and with a compression encoded, reps times; the best times on stdout.
 *
 *   core_harness zstd-big <dir> <width> <level>...
 *       A ZSTD chunk whose zstd frame passes ZSTD_CURRENT_MAX, so that zstd corrects its
 *       indices within it (cmd_zstd_big), at each level; digests on stdout.
 *
 *   core_harness levels <file> <out-dir>
 *       Every level of every part, decoded into buffers of each pixel type, as the C++
 *       library's readers drive Core (cmd_levels); the planes (a deep part's counts and
 *       samples) and a manifest.
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

/* The pixel type conversions, every input of each, through Core's own pipeline: a part of one
 * channel of type T, 65536 pixels wide, one scanline, NO_COMPRESSION, written to <dir>/T.exr.
 * Encoding: its chunk encoded from user data of each other type U, whose 65536 values are one
 * block of U's inputs, the write_fn replaced by one that digests the packed bytes and writes
 * nothing. Decoding: the file's chunk decoded into each other type U, the read_fn wrapped so
 * the packed bytes Core read are replaced by one block of T's inputs. A half's inputs are one
 * block; a float's and a uint's, 65536 (every 32-bit pattern, in order). Each 256 blocks (or
 * the one) give a line "encode|decode <T> <U> <n> <digest>", the digest FNV-1a 64 over the
 * converted bytes taken eight at a time (a little-endian word each): a fingerprint of what
 * Core produced. */

#define CONVERT_WIDTH 65536

static const char*
type_name (exr_pixel_type_t t)
{
    return t == EXR_PIXEL_HALF ? "HALF" : t == EXR_PIXEL_FLOAT ? "FLOAT" : "UINT";
}

static int
type_size (exr_pixel_type_t t)
{
    return t == EXR_PIXEL_HALF ? 2 : 4;
}

/* Block b of a type's inputs, as their little-endian bytes: the values b * 65536 + x. */
static void
fill_block (uint8_t* buf, exr_pixel_type_t t, uint32_t b)
{
    int n = type_size (t);
    for (uint32_t x = 0; x < CONVERT_WIDTH; ++x)
    {
        uint32_t v = (b << 16) | x;
        for (int k = 0; k < n; ++k)
            buf[x * (uint32_t) n + (uint32_t) k] = (uint8_t) (v >> (8 * k));
    }
}

typedef struct
{
    uint64_t         h;
    uint32_t         block;
    exr_pixel_type_t type;
    exr_result_t (*read_fn) (exr_decode_pipeline_t*);
} convert_state_t;

static void
digest_words (uint64_t* h, const uint8_t* p, size_t n)
{
    for (size_t i = 0; i + 8 <= n; i += 8)
    {
        uint64_t w = 0;
        for (int k = 7; k >= 0; --k)
            w = (w << 8) | p[i + (size_t) k];
        *h = (*h ^ w) * 1099511628211ULL;
    }
}

static exr_result_t
digest_write (exr_encode_pipeline_t* enc)
{
    convert_state_t* s = (convert_state_t*) enc->encoding_user_data;
    digest_words (&s->h, (const uint8_t*) enc->packed_buffer, (size_t) enc->packed_bytes);
    return EXR_ERR_SUCCESS;
}

static exr_result_t
block_read (exr_decode_pipeline_t* dec)
{
    convert_state_t* s  = (convert_state_t*) dec->decoding_user_data;
    exr_result_t     rv = s->read_fn (dec);
    if (rv == EXR_ERR_SUCCESS)
    {
        if (dec->chunk.packed_size !=
            (uint64_t) CONVERT_WIDTH * (uint64_t) type_size (s->type))
            return EXR_ERR_CORRUPT_CHUNK;
        fill_block ((uint8_t*) dec->packed_buffer, s->type, s->block);
    }
    return rv;
}

static void
convert_line (
    const char* what, exr_pixel_type_t t, exr_pixel_type_t u, uint32_t n, uint64_t h)
{
    printf (
        "%s %s %s %u %016llx\n",
        what,
        type_name (t),
        type_name (u),
        n,
        (unsigned long long) h);
}

static void
set_user (exr_coding_channel_info_t* ch, exr_pixel_type_t u, uint8_t* ptr, int encode)
{
    if (encode)
        ch->encode_from_ptr = ptr;
    else
        ch->decode_to_ptr = ptr;
    ch->user_pixel_stride      = type_size (u);
    ch->user_line_stride       = CONVERT_WIDTH * type_size (u);
    ch->user_bytes_per_element = (int16_t) type_size (u);
    ch->user_data_type         = (uint16_t) u;
}

static const exr_pixel_type_t convert_types[3] = {
    EXR_PIXEL_UINT, EXR_PIXEL_HALF, EXR_PIXEL_FLOAT};

static int
convert_type (const char* dir, exr_pixel_type_t t)
{
    char             path[4096];
    exr_context_t    f   = NULL;
    int              p   = 0;
    uint8_t*         buf = (uint8_t*) malloc (CONVERT_WIDTH * 4);
    exr_chunk_info_t ci;
    convert_state_t  s;
    if (!buf) return fail ("out of memory", EXR_ERR_OUT_OF_MEMORY);
    snprintf (path, sizeof (path), "%s/%s.exr", dir, type_name (t));

    CHECK (exr_start_write (&f, path, EXR_WRITE_FILE_DIRECTLY, NULL));
    CHECK (exr_add_part (f, "", EXR_STORAGE_SCANLINE, &p));
    CHECK (exr_initialize_required_attr_simple (
        f, p, CONVERT_WIDTH, 1, EXR_COMPRESSION_NONE));
    CHECK (exr_add_channel (f, p, "A", t, EXR_PERCEPTUALLY_LINEAR, 1, 1));
    CHECK (exr_write_header (f));
    CHECK (exr_write_scanline_chunk_info (f, p, 0, &ci));
    for (int k = 0; k < 3; ++k)
    {
        exr_pixel_type_t      u   = convert_types[k];
        uint32_t              nb  = u == EXR_PIXEL_HALF ? 1 : 65536;
        exr_encode_pipeline_t enc = EXR_ENCODE_PIPELINE_INITIALIZER;
        if (u == t) continue;
        CHECK (exr_encoding_initialize (f, p, &ci, &enc));
        set_user (enc.channels, u, buf, 1);
        CHECK (exr_encoding_choose_default_routines (f, p, &enc));
        enc.write_fn           = digest_write;
        enc.encoding_user_data = &s;
        s.h                    = 14695981039346656037ULL;
        for (uint32_t b = 0; b < nb; ++b)
        {
            fill_block (buf, u, b);
            CHECK (exr_encoding_run (f, p, &enc));
            if (nb == 1 || (b & 255) == 255)
            {
                convert_line ("encode", t, u, nb == 1 ? 0 : b >> 8, s.h);
                s.h = 14695981039346656037ULL;
            }
        }
        CHECK (exr_encoding_destroy (f, &enc));
    }
    {
        /* the chunk itself: block 0 of the file's own type */
        exr_encode_pipeline_t enc = EXR_ENCODE_PIPELINE_INITIALIZER;
        CHECK (exr_encoding_initialize (f, p, &ci, &enc));
        set_user (enc.channels, t, buf, 1);
        fill_block (buf, t, 0);
        CHECK (exr_encoding_choose_default_routines (f, p, &enc));
        CHECK (exr_encoding_run (f, p, &enc));
        CHECK (exr_encoding_destroy (f, &enc));
    }
    CHECK (exr_finish (&f));

    CHECK (exr_start_read (&f, path, NULL));
    CHECK (exr_read_scanline_chunk_info (f, 0, 0, &ci));
    for (int k = 0; k < 3; ++k)
    {
        exr_pixel_type_t      u   = convert_types[k];
        uint32_t              nb  = t == EXR_PIXEL_HALF ? 1 : 65536;
        exr_decode_pipeline_t dec = EXR_DECODE_PIPELINE_INITIALIZER;
        if (u == t) continue;
        CHECK (exr_decoding_initialize (f, 0, &ci, &dec));
        set_user (dec.channels, u, buf, 0);
        CHECK (exr_decoding_choose_default_routines (f, 0, &dec));
        s.read_fn              = dec.read_fn;
        s.type                 = t;
        dec.read_fn            = block_read;
        dec.decoding_user_data = &s;
        s.h                    = 14695981039346656037ULL;
        for (uint32_t b = 0; b < nb; ++b)
        {
            s.block = b;
            CHECK (exr_decoding_run (f, 0, &dec));
            digest_words (&s.h, buf, (size_t) CONVERT_WIDTH * (size_t) type_size (u));
            if (nb == 1 || (b & 255) == 255)
            {
                convert_line ("decode", t, u, nb == 1 ? 0 : b >> 8, s.h);
                s.h = 14695981039346656037ULL;
            }
        }
        CHECK (exr_decoding_destroy (f, &dec));
    }
    CHECK (exr_finish (&f));
    free (buf);
    return 0;
}

static int
cmd_convert (int argc, char** argv)
{
    if (argc != 1) return fail ("convert: <dir>", EXR_ERR_INVALID_ARGUMENT);
    for (int k = 0; k < 3; ++k)
    {
        int r = convert_type (argv[0], convert_types[k]);
        if (r) return r;
    }
    fflush (stdout);
    return 0;
}

/* Timing, for comparing the port's speed with Core's (not a check: nothing here is an expected
 * value). A clock of the platform's own: QueryPerformanceCounter, or CLOCK_MONOTONIC. */
#ifdef _WIN32
#    define WIN32_LEAN_AND_MEAN
#    include <windows.h>
static double
bench_now (void)
{
    LARGE_INTEGER c, f;
    QueryPerformanceCounter (&c);
    QueryPerformanceFrequency (&f);
    return (double) c.QuadPart / (double) f.QuadPart;
}
#else
#    include <time.h>
static double
bench_now (void)
{
    struct timespec ts;
    clock_gettime (CLOCK_MONOTONIC, &ts);
    return (double) ts.tv_sec + (double) ts.tv_nsec * 1e-9;
}
#endif

static exr_result_t
bench_write_nothing (exr_encode_pipeline_t* enc)
{
    (void) enc;
    return EXR_ERR_SUCCESS;
}

typedef struct
{
    exr_chunk_info_t ci;
    planes_t         planes; /* the encoder's input (encode chunks only) */
} bench_chunk_t;

static int
bench_push (bench_chunk_t** v, size_t* n, size_t* cap, const exr_chunk_info_t* ci)
{
    if (*n == *cap)
    {
        size_t         ncap = *cap ? *cap * 2 : 64;
        bench_chunk_t* nv   = (bench_chunk_t*) realloc (*v, ncap * sizeof (bench_chunk_t));
        if (!nv) return fail ("out of memory", EXR_ERR_OUT_OF_MEMORY);
        *v   = nv;
        *cap = ncap;
    }
    memset (*v + *n, 0, sizeof (bench_chunk_t));
    (*v)[*n].ci = *ci;
    ++*n;
    return 0;
}

/* Every chunk of part 0 in the order recompress writes them, read side (in) or write side
 * (out, when not NULL). */
static int
bench_chunks (
    exr_const_context_t in, exr_context_t out, bench_chunk_t** v, size_t* n)
{
    size_t        cap = 0;
    exr_storage_t st;
    *v = NULL;
    *n = 0;
    CHECK (exr_get_storage (in, 0, &st));
    if (st == EXR_STORAGE_SCANLINE)
    {
        exr_attr_box2i_t dw;
        int32_t          lpc;
        CHECK (exr_get_data_window (in, 0, &dw));
        CHECK (exr_get_scanlines_per_chunk (out ? out : in, 0, &lpc));
        for (int64_t y = dw.min.y; y <= dw.max.y; y += lpc)
        {
            exr_chunk_info_t ci;
            int              r;
            if (out)
                CHECK (exr_write_scanline_chunk_info (out, 0, (int) y, &ci));
            else
                CHECK (exr_read_scanline_chunk_info (in, 0, (int) y, &ci));
            if ((r = bench_push (v, n, &cap, &ci))) return r;
        }
        return 0;
    }
    if (st != EXR_STORAGE_TILED) return fail ("bench: deep parts are not supported", EXR_ERR_INVALID_ARGUMENT);
    {
        int32_t               levelsx, levelsy;
        uint32_t              tw, th;
        exr_tile_level_mode_t mode;
        exr_tile_round_mode_t round;
        CHECK (exr_get_tile_descriptor (in, 0, &tw, &th, &mode, &round));
        CHECK (exr_get_tile_levels (in, 0, &levelsx, &levelsy));
        for (int ly = 0; ly < levelsy; ++ly)
        {
            for (int lx = 0; lx < levelsx; ++lx)
            {
                int32_t cx, cy;
                if (mode != EXR_TILE_RIPMAP_LEVELS && lx != ly) continue;
                CHECK (exr_get_tile_counts (in, 0, lx, ly, &cx, &cy));
                for (int ty = 0; ty < cy; ++ty)
                {
                    for (int tx = 0; tx < cx; ++tx)
                    {
                        exr_chunk_info_t ci;
                        int              r;
                        if (out)
                            CHECK (exr_write_tile_chunk_info (out, 0, tx, ty, lx, ly, &ci));
                        else
                            CHECK (exr_read_tile_chunk_info (in, 0, tx, ty, lx, ly, &ci));
                        if ((r = bench_push (v, n, &cap, &ci))) return r;
                    }
                }
            }
        }
    }
    return 0;
}

/* One chunk decoded into planar buffers of the channels' own types, at planes. */
static int
bench_decode (exr_const_context_t in, const exr_chunk_info_t* ci, planes_t* planes, int alloc)
{
    exr_decode_pipeline_t dec = EXR_DECODE_PIPELINE_INITIALIZER;
    int                   r;
    CHECK (exr_decoding_initialize (in, 0, ci, &dec));
    if (alloc && (r = plan_channels (dec.channels, dec.channel_count, planes))) return r;
    {
        size_t off = 0;
        for (int16_t c = 0; c < dec.channel_count; ++c)
        {
            exr_coding_channel_info_t* ch = dec.channels + c;
            ch->decode_to_ptr          = planes->data + off;
            ch->user_pixel_stride      = ch->bytes_per_element;
            ch->user_line_stride       = ch->width * ch->bytes_per_element;
            ch->user_bytes_per_element = ch->bytes_per_element;
            ch->user_data_type         = ch->data_type;
            off += (size_t) ch->width * (size_t) ch->height * (size_t) ch->bytes_per_element;
        }
    }
    CHECK (exr_decoding_choose_default_routines (in, 0, &dec));
    CHECK (exr_decoding_run (in, 0, &dec));
    CHECK (exr_decoding_destroy (in, &dec));
    return 0;
}

/* core_harness bench <in> <reps> [<compression> [--zip N] [--zstd N] [--dwa F]]
 * Part 0 of <in>: every chunk decoded (into planar buffers of the channels' own types) reps
 * times; with a compression, every chunk of the rewrite recompress makes encoded reps times,
 * from planes filled beforehand, with a write_fn that writes nothing. Prints the best time
 * of each, in seconds. Single-threaded. */
static int
cmd_bench (int argc, char** argv)
{
    exr_context_t  in = NULL, out = NULL;
    bench_chunk_t* rd = NULL;
    bench_chunk_t* wr = NULL;
    size_t         nrd = 0, nwr = 0, maxsz = 0;
    planes_t       scratch;
    int            reps, r;
    double         best = 1e30;

    if (argc < 2) return fail ("bench: <in> <reps> [<compression> ...]", EXR_ERR_INVALID_ARGUMENT);
    reps = atoi (argv[1]);
    CHECK (exr_start_read (&in, argv[0], NULL));
    if ((r = bench_chunks (in, NULL, &rd, &nrd))) return r;
    for (size_t i = 0; i < nrd; ++i)
        if (rd[i].ci.unpacked_size > maxsz) maxsz = (size_t) rd[i].ci.unpacked_size;
    scratch.data = (uint8_t*) calloc (maxsz + 1, 1);
    if (!scratch.data) return fail ("out of memory", EXR_ERR_OUT_OF_MEMORY);
    for (int k = 0; k < reps; ++k)
    {
        double t0 = bench_now (), t;
        for (size_t i = 0; i < nrd; ++i)
            if ((r = bench_decode (in, &rd[i].ci, &scratch, 0))) return r;
        t = bench_now () - t0;
        if (t < best) best = t;
    }
    printf ("decode %zu %.9f\n", nrd, best);

    if (argc >= 3)
    {
        exr_compression_t comp = (exr_compression_t) atoi (argv[2]);
        exr_storage_t     st;
        char              tmp[4096];
        CHECK (exr_get_storage (in, 0, &st));
        snprintf (tmp, sizeof (tmp), "%s.bench-out", argv[0]);
        CHECK (exr_start_write (&out, tmp, EXR_WRITE_FILE_DIRECTLY, NULL));
        {
            int idx = -1;
            CHECK (exr_add_part (out, NULL, st, &idx));
            CHECK (exr_copy_unset_attributes (out, idx, in, 0));
            CHECK (exr_set_compression (out, idx, comp));
            /* no chunk is written, so none in order: RANDOM_Y lets each encode (chunk.c:2415) */
            CHECK (exr_set_lineorder (out, idx, EXR_LINEORDER_RANDOM_Y));
            for (int i = 3; i + 1 < argc; i += 2)
            {
                if (!strcmp (argv[i], "--zip"))
                    CHECK (exr_set_zip_compression_level (out, idx, atoi (argv[i + 1])));
                else if (!strcmp (argv[i], "--zstd"))
                    CHECK (exr_set_zstd_compression_level (out, idx, atoi (argv[i + 1])));
                else if (!strcmp (argv[i], "--dwa"))
                    CHECK (exr_set_dwa_compression_level (out, idx, (float) atof (argv[i + 1])));
                else
                    return fail ("bench: unknown option", EXR_ERR_INVALID_ARGUMENT);
            }
        }
        CHECK (exr_write_header (out));
        if ((r = bench_chunks (in, out, &wr, &nwr))) return r;

        /* the encoders' inputs, filled beforehand */
        if (st == EXR_STORAGE_SCANLINE)
        {
            /* the whole part decoded, then each write chunk's rows copied out */
            const exr_attr_chlist_t* chans = NULL;
            exr_attr_box2i_t         dw;
            uint8_t*                 whole[256];
            int64_t                  widths[256];
            int                      bpe[256];
            CHECK (exr_get_data_window (in, 0, &dw));
            CHECK (exr_get_channels (in, 0, &chans));
            if (chans->num_channels > 256) return fail ("too many channels", EXR_ERR_INVALID_ARGUMENT);
            for (int c = 0; c < chans->num_channels; ++c)
            {
                const exr_attr_chlist_entry_t* e = chans->entries + c;
                int64_t h = sampled (dw.min.y, (int64_t) dw.max.y + 1, e->y_sampling);
                widths[c] = sampled (dw.min.x, (int64_t) dw.max.x + 1, e->x_sampling);
                bpe[c]    = e->pixel_type == EXR_PIXEL_HALF ? 2 : 4;
                whole[c]  = (uint8_t*) calloc ((size_t) (widths[c] * h * bpe[c]) + 1, 1);
                if (!whole[c]) return fail ("out of memory", EXR_ERR_OUT_OF_MEMORY);
            }
            for (size_t i = 0; i < nrd; ++i)
            {
                exr_decode_pipeline_t dec = EXR_DECODE_PIPELINE_INITIALIZER;
                CHECK (exr_decoding_initialize (in, 0, &rd[i].ci, &dec));
                for (int16_t c = 0; c < dec.channel_count; ++c)
                {
                    exr_coding_channel_info_t*     ch = dec.channels + c;
                    const exr_attr_chlist_entry_t* e  = chans->entries + c;
                    int64_t row = sampled (dw.min.y, rd[i].ci.start_y, e->y_sampling);
                    ch->decode_to_ptr          = whole[c] + row * widths[c] * bpe[c];
                    ch->user_pixel_stride      = ch->bytes_per_element;
                    ch->user_line_stride       = (int32_t) (widths[c] * bpe[c]);
                    ch->user_bytes_per_element = ch->bytes_per_element;
                    ch->user_data_type         = ch->data_type;
                }
                CHECK (exr_decoding_choose_default_routines (in, 0, &dec));
                CHECK (exr_decoding_run (in, 0, &dec));
                CHECK (exr_decoding_destroy (in, &dec));
            }
            for (size_t i = 0; i < nwr; ++i)
            {
                exr_encode_pipeline_t enc = EXR_ENCODE_PIPELINE_INITIALIZER;
                size_t                off = 0;
                CHECK (exr_encoding_initialize (out, 0, &wr[i].ci, &enc));
                if ((r = plan_channels (enc.channels, enc.channel_count, &wr[i].planes))) return r;
                for (int16_t c = 0; c < enc.channel_count; ++c)
                {
                    const exr_coding_channel_info_t* ch = enc.channels + c;
                    const exr_attr_chlist_entry_t*   e  = chans->entries + c;
                    int64_t row = sampled (dw.min.y, wr[i].ci.start_y, e->y_sampling);
                    size_t  n   = (size_t) ch->width * (size_t) ch->height * (size_t) bpe[c];
                    memcpy (wr[i].planes.data + off, whole[c] + row * widths[c] * bpe[c], n);
                    off += n;
                }
                CHECK (exr_encoding_destroy (out, &enc));
            }
            for (int c = 0; c < chans->num_channels; ++c)
                free (whole[c]);
        }
        else
        {
            /* each tile decoded, its rows copied into planes of the write side's sizes (they
             * differ at a level's edge: I-21) */
            for (size_t i = 0; i < nwr; ++i)
            {
                exr_decode_pipeline_t dec = EXR_DECODE_PIPELINE_INITIALIZER;
                exr_encode_pipeline_t enc = EXR_ENCODE_PIPELINE_INITIALIZER;
                size_t                doff = 0, eoff = 0;
                if ((r = bench_decode (in, &rd[i].ci, &scratch, 0))) return r;
                CHECK (exr_decoding_initialize (in, 0, &rd[i].ci, &dec));
                CHECK (exr_encoding_initialize (out, 0, &wr[i].ci, &enc));
                if ((r = plan_channels (enc.channels, enc.channel_count, &wr[i].planes))) return r;
                for (int16_t c = 0; c < enc.channel_count; ++c)
                {
                    const exr_coding_channel_info_t* d = dec.channels + c;
                    const exr_coding_channel_info_t* e = enc.channels + c;
                    int32_t rows = d->height < e->height ? d->height : e->height;
                    int32_t cols = d->width < e->width ? d->width : e->width;
                    for (int32_t y = 0; y < rows; ++y)
                        memcpy (
                            wr[i].planes.data + eoff +
                                (size_t) y * (size_t) e->width * e->bytes_per_element,
                            scratch.data + doff +
                                (size_t) y * (size_t) d->width * d->bytes_per_element,
                            (size_t) cols * d->bytes_per_element);
                    doff += (size_t) d->width * (size_t) d->height * d->bytes_per_element;
                    eoff += (size_t) e->width * (size_t) e->height * e->bytes_per_element;
                }
                CHECK (exr_encoding_destroy (out, &enc));
                CHECK (exr_decoding_destroy (in, &dec));
            }
        }

        best = 1e30;
        for (int k = 0; k < reps; ++k)
        {
            double t0 = bench_now (), t;
            for (size_t i = 0; i < nwr; ++i)
            {
                exr_encode_pipeline_t enc = EXR_ENCODE_PIPELINE_INITIALIZER;
                size_t                off = 0;
                CHECK (exr_encoding_initialize (out, 0, &wr[i].ci, &enc));
                for (int16_t c = 0; c < enc.channel_count; ++c)
                {
                    exr_coding_channel_info_t* ch = enc.channels + c;
                    ch->encode_from_ptr        = wr[i].planes.data + off;
                    ch->user_pixel_stride      = ch->bytes_per_element;
                    ch->user_line_stride       = ch->width * ch->bytes_per_element;
                    ch->user_bytes_per_element = ch->bytes_per_element;
                    ch->user_data_type         = ch->data_type;
                    off += (size_t) ch->width * (size_t) ch->height *
                           (size_t) ch->bytes_per_element;
                }
                CHECK (exr_encoding_choose_default_routines (out, 0, &enc));
                enc.write_fn = bench_write_nothing;
                CHECK (exr_encoding_run (out, 0, &enc));
                CHECK (exr_encoding_destroy (out, &enc));
            }
            t = bench_now () - t0;
            if (t < best) best = t;
        }
        printf ("encode %zu %.9f\n", nwr, best);
        for (size_t i = 0; i < nwr; ++i)
            free (wr[i].planes.data);
        free (wr);
        exr_finish (&out);
        remove (tmp);
    }
    free (scratch.data);
    free (rd);
    CHECK (exr_finish (&in));
    fflush (stdout);
    return 0;
}

/* A ZSTD chunk whose zstd frame passes ZSTD_CURRENT_MAX (3500 MiB), so that zstd corrects
 * its indices within it: a scanline part one line high, <width> pixels wide, of two HALF
 * channels (A, B) and one FLOAT (C), whose inner stream (the half segment, then the float
 * one, each at most INT_MAX bytes) is 8 * <width> + 16 bytes. Its pixels are integer hashes
 * of the column with several periods (zstd_big_value), so that every level's window holds
 * matches. For each level in turn, in one process (the thread's zstd context reused, as
 * Core's chunks reuse it): the chunk encoded through Core's pipeline with a write_fn that
 * digests the compressed bytes (FNV-1a 64) and writes nothing; one line each,
 * "zstd <level> <packed bytes> <compressed bytes> <digest>". */

static uint32_t
zstd_big_hash (uint32_t v)
{
    v ^= v >> 16;
    v *= 0x7feb352dU;
    v ^= v >> 15;
    v *= 0x846ca68bU;
    v ^= v >> 16;
    return v;
}

/* channel c's value at column x: the low bits of a hash of x modulo a period, with a slow
 * drift every so many columns, so that runs repeat but not forever */
static uint32_t
zstd_big_value (int c, uint32_t x)
{
    if (c == 0) return zstd_big_hash (x % 65521U) & 0x3FFFU;
    if (c == 1) return (zstd_big_hash ((x % 40009U) + x / 1000003U) >> 7) & 0xFFFFU;
    return 0x3F800000U | (zstd_big_hash (x % 30011U) & 0x007FFF00U) | ((x >> 13) & 0xFFU);
}

typedef struct
{
    uint64_t h;
    uint64_t n;
} zstd_big_state_t;

static exr_result_t
zstd_big_write (exr_encode_pipeline_t* enc)
{
    zstd_big_state_t* s = (zstd_big_state_t*) enc->encoding_user_data;
    const uint8_t*    p = (const uint8_t*) enc->compressed_buffer;
    for (uint64_t i = 0; i < enc->compressed_bytes; ++i)
        s->h = (s->h ^ p[i]) * 1099511628211ULL;
    s->n = enc->compressed_bytes;
    return EXR_ERR_SUCCESS;
}

static int
cmd_zstd_big (int argc, char** argv)
{
    char          path[4096];
    exr_context_t f   = NULL;
    int           p   = 0;
    uint32_t      w;
    uint8_t*      planes[3];
    if (argc < 3) return fail ("zstd-big: <dir> <width> <level>...", EXR_ERR_INVALID_ARGUMENT);
    w = (uint32_t) strtoul (argv[1], NULL, 10);
    for (int c = 0; c < 3; ++c)
    {
        size_t bpe = c == 2 ? 4 : 2;
        planes[c]  = (uint8_t*) malloc ((size_t) w * bpe);
        if (!planes[c]) return fail ("out of memory", EXR_ERR_OUT_OF_MEMORY);
        for (uint32_t x = 0; x < w; ++x)
        {
            uint32_t v = zstd_big_value (c, x);
            for (size_t k = 0; k < bpe; ++k)
                planes[c][(size_t) x * bpe + k] = (uint8_t) (v >> (8 * k));
        }
    }
    snprintf (path, sizeof (path), "%s/zstd-big.exr", argv[0]);
    for (int l = 2; l < argc; ++l)
    {
        int                   level = atoi (argv[l]);
        exr_chunk_info_t      ci;
        exr_encode_pipeline_t enc = EXR_ENCODE_PIPELINE_INITIALIZER;
        zstd_big_state_t      s   = {14695981039346656037ULL, 0};
        CHECK (exr_start_write (&f, path, EXR_WRITE_FILE_DIRECTLY, NULL));
        CHECK (exr_add_part (f, "", EXR_STORAGE_SCANLINE, &p));
        CHECK (exr_initialize_required_attr_simple (f, p, (int32_t) w, 1, EXR_COMPRESSION_ZSTD));
        CHECK (exr_add_channel (f, p, "A", EXR_PIXEL_HALF, EXR_PERCEPTUALLY_LINEAR, 1, 1));
        CHECK (exr_add_channel (f, p, "B", EXR_PIXEL_HALF, EXR_PERCEPTUALLY_LINEAR, 1, 1));
        CHECK (exr_add_channel (f, p, "C", EXR_PIXEL_FLOAT, EXR_PERCEPTUALLY_LINEAR, 1, 1));
        CHECK (exr_set_zstd_compression_level (f, p, level));
        CHECK (exr_write_header (f));
        CHECK (exr_write_scanline_chunk_info (f, p, 0, &ci));
        CHECK (exr_encoding_initialize (f, p, &ci, &enc));
        for (int c = 0; c < 3; ++c)
        {
            exr_coding_channel_info_t* ch = enc.channels + c;
            ch->encode_from_ptr           = planes[c];
            ch->user_pixel_stride         = ch->bytes_per_element;
            ch->user_line_stride          = (int32_t) ((size_t) w * (size_t) ch->bytes_per_element);
            ch->user_bytes_per_element    = ch->bytes_per_element;
            ch->user_data_type            = ch->data_type;
        }
        CHECK (exr_encoding_choose_default_routines (f, p, &enc));
        enc.write_fn           = zstd_big_write;
        enc.encoding_user_data = &s;
        CHECK (exr_encoding_run (f, p, &enc));
        printf (
            "zstd %d %llu %llu %016llx\n",
            level,
            (unsigned long long) enc.packed_bytes,
            (unsigned long long) s.n,
            (unsigned long long) s.h);
        fflush (stdout);
        CHECK (exr_encoding_destroy (f, &enc));
        exr_finish (&f);
        remove (path);
    }
    for (int c = 0; c < 3; ++c)
        free (planes[c]);
    return 0;
}

/* Port-independent output of `levels`: one plane of a part's level, decoded. */
static void
levels_write (
    FILE*                          manifest,
    const char*                    dir,
    int                            p,
    int                            lx,
    int                            ly,
    exr_pixel_type_t               t,
    int                            c,
    const exr_attr_chlist_entry_t* e,
    const uint8_t*                 plane,
    size_t                         bytes,
    int64_t                        h,
    int64_t                        w)
{
    char  path[4096];
    FILE* fh;
    snprintf (path, sizeof (path), "%s/%d.%d.%d.%s.%d.bin", dir, p, lx, ly, type_name (t), c);
    fh = fopen (path, "wb");
    if (fh)
    {
        fwrite (plane, 1, bytes, fh);
        fclose (fh);
    }
    fprintf (manifest, "%d\t%d\t%d\t%s\t%d\t", p, lx, ly, type_name (t), c);
    for (int32_t k = 0; k < e->name.length; ++k)
        fprintf (manifest, "%02x", (unsigned char) e->name.str[k]);
    fprintf (
        manifest,
        "\t%lld\t%lld\t%d.%d.%d.%s.%d.bin\n",
        (long long) h,
        (long long) w,
        p,
        lx,
        ly,
        type_name (t),
        c);
}

/* Points channel c of a decode at `ptr`, to be stored as type t, `line` bytes a row; a
 * channel without lines in the chunk is skipped, as the C++ library's readers skip it. */
static void
levels_point (exr_coding_channel_info_t* ch, exr_pixel_type_t t, uint8_t* ptr, int64_t line)
{
    if (ch->height == 0)
    {
        ch->decode_to_ptr     = NULL;
        ch->user_pixel_stride = 0;
        ch->user_line_stride  = 0;
        return;
    }
    ch->decode_to_ptr          = ptr;
    ch->user_bytes_per_element = (int16_t) type_size (t);
    ch->user_data_type         = (uint16_t) t;
    ch->user_pixel_stride      = type_size (t);
    ch->user_line_stride       = (int32_t) line;
}

static exr_result_t
levels_run (exr_const_context_t f, int p, exr_decode_pipeline_t* dec)
{
    exr_result_t rv = exr_decoding_choose_default_routines (f, p, dec);
    if (rv == EXR_ERR_SUCCESS) rv = exr_decoding_run (f, p, dec);
    exr_decoding_destroy (f, dec);
    return rv;
}

/* One level (a scanline part's only one) of part p, decoded as type t into `planes`, one per
 * channel, `widths[c]` samples a row. */
static exr_result_t
levels_decode (
    exr_const_context_t      f,
    int                      p,
    int                      tiled,
    int                      lx,
    int                      ly,
    exr_pixel_type_t         t,
    const exr_attr_chlist_t* chans,
    exr_attr_box2i_t         dw,
    uint8_t**                planes,
    const int64_t*           widths)
{
    exr_result_t rv;
    if (!tiled)
    {
        int32_t lpc;
        rv = exr_get_scanlines_per_chunk (f, p, &lpc);
        if (rv) return rv;
        for (int64_t y = dw.min.y; y <= dw.max.y; y += lpc)
        {
            exr_chunk_info_t      cin;
            exr_decode_pipeline_t dec = EXR_DECODE_PIPELINE_INITIALIZER;
            rv = exr_read_scanline_chunk_info (f, p, (int) y, &cin);
            if (rv) return rv;
            rv = exr_decoding_initialize (f, p, &cin, &dec);
            if (rv) return rv;
            for (int16_t c = 0; c < dec.channel_count; ++c)
            {
                const exr_attr_chlist_entry_t* e = chans->entries + c;
                int64_t row = sampled (dw.min.y, cin.start_y, e->y_sampling);
                int64_t line = widths[c] * type_size (t);
                levels_point (dec.channels + c, t, planes[c] + row * line, line);
            }
            rv = levels_run (f, p, &dec);
            if (rv) return rv;
        }
        return EXR_ERR_SUCCESS;
    }
    {
        int32_t cx, cy, tw, th;
        rv = exr_get_tile_counts (f, p, lx, ly, &cx, &cy);
        if (rv) return rv;
        rv = exr_get_tile_sizes (f, p, lx, ly, &tw, &th);
        if (rv) return rv;
        for (int ty = 0; ty < cy; ++ty)
        {
            for (int tx = 0; tx < cx; ++tx)
            {
                exr_chunk_info_t      cin;
                exr_decode_pipeline_t dec = EXR_DECODE_PIPELINE_INITIALIZER;
                rv = exr_read_tile_chunk_info (f, p, tx, ty, lx, ly, &cin);
                if (rv) return rv;
                rv = exr_decoding_initialize (f, p, &cin, &dec);
                if (rv) return rv;
                for (int16_t c = 0; c < dec.channel_count; ++c)
                {
                    int64_t line = widths[c] * type_size (t);
                    uint8_t* at  = planes[c] + (int64_t) ty * th * line +
                                  (int64_t) tx * tw * type_size (t);
                    levels_point (dec.channels + c, t, at, line);
                }
                rv = levels_run (f, p, &dec);
                if (rv) return rv;
            }
        }
    }
    return EXR_ERR_SUCCESS;
}

/* The chunks of a level of part p: each one's chunk info and first pixel in the level (a
 * scanline part's one level, a tiled part's level (lx, ly)). */
typedef struct
{
    exr_chunk_info_t ci;
    int64_t          x0, y0;
} deep_chunk_t;

static exr_result_t
deep_chunks (
    exr_const_context_t f,
    int                 p,
    int                 tiled,
    int                 lx,
    int                 ly,
    exr_attr_box2i_t    dw,
    deep_chunk_t**      out,
    int64_t*            n)
{
    exr_result_t rv;
    *n   = 0;
    *out = NULL;
    if (!tiled)
    {
        int32_t lpc;
        int64_t cap;
        rv = exr_get_scanlines_per_chunk (f, p, &lpc);
        if (rv) return rv;
        cap  = ((int64_t) dw.max.y - dw.min.y) / lpc + 1;
        *out = (deep_chunk_t*) calloc ((size_t) cap, sizeof (deep_chunk_t));
        if (!*out) return EXR_ERR_OUT_OF_MEMORY;
        for (int64_t y = dw.min.y; y <= dw.max.y; y += lpc)
        {
            deep_chunk_t* d = *out + (*n)++;
            rv = exr_read_scanline_chunk_info (f, p, (int) y, &d->ci);
            if (rv) return rv;
            d->x0 = 0;
            d->y0 = (int64_t) d->ci.start_y - dw.min.y;
        }
        return EXR_ERR_SUCCESS;
    }
    {
        int32_t cx, cy, tw, th;
        rv = exr_get_tile_counts (f, p, lx, ly, &cx, &cy);
        if (rv) return rv;
        rv = exr_get_tile_sizes (f, p, lx, ly, &tw, &th);
        if (rv) return rv;
        *out = (deep_chunk_t*) calloc ((size_t) cx * (size_t) cy + 1, sizeof (deep_chunk_t));
        if (!*out) return EXR_ERR_OUT_OF_MEMORY;
        for (int ty = 0; ty < cy; ++ty)
            for (int tx = 0; tx < cx; ++tx)
            {
                deep_chunk_t* d = *out + (*n)++;
                rv = exr_read_tile_chunk_info (f, p, tx, ty, lx, ly, &d->ci);
                if (rv) return rv;
                d->x0 = (int64_t) tx * tw;
                d->y0 = (int64_t) ty * th;
            }
    }
    return EXR_ERR_SUCCESS;
}

/* A deep level of part p read as type t, as the C++ library's deep readers drive Core: every
 * chunk's sample counts first (EXR_DECODE_SAMPLE_DATA_ONLY) into `counts` (lw by lh), then
 * every chunk's samples through tables of pointers, one a pixel, into `samples[c]`, each
 * pixel's samples after the previous pixel's (EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS, the
 * counts individual). */
static exr_result_t
levels_decode_deep (
    exr_const_context_t f,
    int                 p,
    int                 tiled,
    int                 lx,
    int                 ly,
    exr_pixel_type_t    t,
    int                 nchan,
    exr_attr_box2i_t    dw,
    int64_t             lw,
    int64_t             lh,
    uint32_t*           counts,
    uint8_t**           samples,
    uint64_t*           total)
{
    deep_chunk_t* chunks;
    int64_t       n;
    uint64_t*     starts;
    exr_result_t  rv = deep_chunks (f, p, tiled, lx, ly, dw, &chunks, &n);
    if (rv)
    {
        free (chunks);
        return rv;
    }
    for (int64_t k = 0; k < n && !rv; ++k)
    {
        deep_chunk_t*         d   = chunks + k;
        exr_decode_pipeline_t dec = EXR_DECODE_PIPELINE_INITIALIZER;
        rv = exr_decoding_initialize (f, p, &d->ci, &dec);
        if (rv) break;
        dec.decode_flags |= EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS |
                            EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL |
                            EXR_DECODE_SAMPLE_DATA_ONLY;
        rv = exr_decoding_choose_default_routines (f, p, &dec);
        if (rv == EXR_ERR_SUCCESS) rv = exr_decoding_run (f, p, &dec);
        if (rv == EXR_ERR_SUCCESS)
            for (int32_t y = 0; y < d->ci.height; ++y)
                for (int32_t x = 0; x < d->ci.width; ++x)
                    counts[(d->y0 + y) * lw + d->x0 + x] =
                        (uint32_t) dec.sample_count_table[(int64_t) y * d->ci.width + x];
        exr_decoding_destroy (f, &dec);
    }
    if (rv)
    {
        free (chunks);
        return rv;
    }
    starts = (uint64_t*) malloc ((size_t) (lw * lh + 1) * sizeof (uint64_t));
    if (!starts)
    {
        free (chunks);
        return EXR_ERR_OUT_OF_MEMORY;
    }
    *total = 0;
    for (int64_t i = 0; i < lw * lh; ++i)
    {
        starts[i] = *total;
        *total += counts[i];
    }
    for (int c = 0; c < nchan; ++c)
    {
        samples[c] = (uint8_t*) calloc ((size_t) (*total * type_size (t)) + 1, 1);
        if (!samples[c]) rv = EXR_ERR_OUT_OF_MEMORY;
    }
    for (int64_t k = 0; k < n && !rv; ++k)
    {
        deep_chunk_t*         d   = chunks + k;
        exr_decode_pipeline_t dec = EXR_DECODE_PIPELINE_INITIALIZER;
        void**                tables[256];
        int64_t               cw = d->ci.width, ch = d->ci.height;
        int16_t               nc;
        rv = exr_decoding_initialize (f, p, &d->ci, &dec);
        if (rv) break;
        nc = dec.channel_count;
        dec.decode_flags |= EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS |
                            EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL;
        for (int16_t c = 0; c < dec.channel_count; ++c)
        {
            exr_coding_channel_info_t* cc = dec.channels + c;
            tables[c] = NULL;
            if (cc->height == 0)
            {
                cc->decode_to_ptr     = NULL;
                cc->user_pixel_stride = 0;
                cc->user_line_stride  = 0;
                continue;
            }
            tables[c] = (void**) malloc ((size_t) (cw * ch + 1) * sizeof (void*));
            if (!tables[c]) return EXR_ERR_OUT_OF_MEMORY;
            for (int64_t y = 0; y < ch; ++y)
                for (int64_t x = 0; x < cw; ++x)
                    tables[c][y * cw + x] =
                        samples[c] + starts[(d->y0 + y) * lw + d->x0 + x] * type_size (t);
            cc->decode_to_ptr          = (uint8_t*) tables[c];
            cc->user_bytes_per_element = (int16_t) type_size (t);
            cc->user_data_type         = (uint16_t) t;
            cc->user_pixel_stride      = (int32_t) sizeof (void*);
            cc->user_line_stride       = (int32_t) (sizeof (void*) * cw);
        }
        rv = levels_run (f, p, &dec);
        for (int16_t c = 0; c < nc; ++c)
            free (tables[c]);
    }
    free (starts);
    free (chunks);
    return rv;
}

/* core_harness levels <file> <out-dir>: what Core gives the C++ library's readers where the
 * wheel cannot look: every level of a tiled part, and every pixel type the channels can be
 * read as. For each part p, each output type t (half, float, uint) and each level (lx,
 * ly) of a tiled part (a scanline part has the one, 0 0), the level is decoded chunk by
 * chunk with Core's default routines into one planar buffer per channel of type t (its
 * samples row by row), as `InputPart::readPixels` and `readTiles` drive Core, and written to
 * <out-dir>/<p>.<lx>.<ly>.<t>.<c>.bin; <out-dir>/manifest.tsv gets one line a channel,
 *   p  lx  ly  t  c  name (hex)  height  width  file
 * or, where a chunk of the level fails, `p lx ly t error <code name>`. A deep part's level
 * (levels_decode_deep) also writes its sample counts, <p>.<lx>.<ly>.<t>.counts (uint32, row by
 * row), and each channel's file holds its samples, each pixel's after the previous pixel's. */
static int
cmd_levels (int argc, char** argv)
{
    exr_context_t                f;
    exr_context_initializer_t    init = EXR_DEFAULT_CONTEXT_INITIALIZER;
    int                          np;
    char                         path[4096];
    FILE*                        manifest;
    static const exr_pixel_type_t types[3] = {EXR_PIXEL_HALF, EXR_PIXEL_FLOAT, EXR_PIXEL_UINT};

    if (argc != 2) return fail ("levels: <file> <out-dir>", EXR_ERR_INVALID_ARGUMENT);
    snprintf (path, sizeof (path), "%s/manifest.tsv", argv[1]);
    manifest = fopen (path, "w");
    if (!manifest) return fail ("levels: cannot write the manifest", EXR_ERR_FILE_ACCESS);
    {
        exr_result_t rv = exr_start_read (&f, argv[0], &init);
        if (rv)
        {
            fprintf (manifest, "error\t%s\n", exr_get_error_code_as_string (rv));
            fclose (manifest);
            return 0;
        }
    }
    CHECK (exr_get_count (f, &np));
    for (int p = 0; p < np; ++p)
    {
        exr_storage_t            st;
        exr_attr_box2i_t         dw;
        const exr_attr_chlist_t* chans = NULL;
        int32_t                  nlx = 1, nly = 1;
        uint32_t                 txs, tys;
        exr_tile_level_mode_t    mode = EXR_TILE_ONE_LEVEL;
        exr_tile_round_mode_t    round;
        int                      tiled, deep;

        CHECK (exr_get_storage (f, p, &st));
        if (st != EXR_STORAGE_SCANLINE && st != EXR_STORAGE_TILED &&
            st != EXR_STORAGE_DEEP_SCANLINE && st != EXR_STORAGE_DEEP_TILED)
            continue;
        tiled = st == EXR_STORAGE_TILED || st == EXR_STORAGE_DEEP_TILED;
        deep  = st == EXR_STORAGE_DEEP_SCANLINE || st == EXR_STORAGE_DEEP_TILED;
        CHECK (exr_get_data_window (f, p, &dw));
        CHECK (exr_get_channels (f, p, &chans));
        if (chans->num_channels > 256)
            return fail ("too many channels", EXR_ERR_INVALID_ARGUMENT);
        if (tiled)
        {
            CHECK (exr_get_tile_levels (f, p, &nlx, &nly));
            CHECK (exr_get_tile_descriptor (f, p, &txs, &tys, &mode, &round));
        }
        for (int k = 0; k < 3; ++k)
        {
            exr_pixel_type_t t = types[k];
            for (int ly = 0; ly < nly; ++ly)
            {
                for (int lx = 0; lx < nlx; ++lx)
                {
                    uint8_t*     planes[256];
                    int64_t      widths[256], heights[256];
                    exr_result_t rv = EXR_ERR_SUCCESS;
                    int32_t      lw = 0, lh = 0;

                    if (mode == EXR_TILE_MIPMAP_LEVELS && lx != ly) continue;
                    if (tiled)
                    {
                        CHECK (exr_get_level_sizes (f, p, lx, ly, &lw, &lh));
                    }
                    if (deep)
                    {
                        int64_t   w = tiled ? lw : (int64_t) dw.max.x - dw.min.x + 1;
                        int64_t   h = tiled ? lh : (int64_t) dw.max.y - dw.min.y + 1;
                        uint32_t* counts = (uint32_t*) calloc ((size_t) (w * h) + 1, 4);
                        uint64_t  total  = 0;
                        if (!counts) return fail ("out of memory", EXR_ERR_OUT_OF_MEMORY);
                        for (int c = 0; c < chans->num_channels; ++c)
                            planes[c] = NULL;
                        rv = levels_decode_deep (
                            f, p, tiled, lx, ly, t, chans->num_channels, dw, w, h, counts,
                            planes, &total);
                        if (rv)
                            fprintf (
                                manifest,
                                "%d\t%d\t%d\t%s\terror\t%s\n",
                                p,
                                lx,
                                ly,
                                type_name (t),
                                exr_get_error_code_as_string (rv));
                        else
                        {
                            FILE* fh;
                            snprintf (
                                path,
                                sizeof (path),
                                "%s/%d.%d.%d.%s.counts",
                                argv[1],
                                p,
                                lx,
                                ly,
                                type_name (t));
                            fh = fopen (path, "wb");
                            if (fh)
                            {
                                fwrite (counts, 4, (size_t) (w * h), fh);
                                fclose (fh);
                            }
                            for (int c = 0; c < chans->num_channels; ++c)
                                levels_write (
                                    manifest,
                                    argv[1],
                                    p,
                                    lx,
                                    ly,
                                    t,
                                    c,
                                    chans->entries + c,
                                    planes[c],
                                    (size_t) (total * (uint64_t) type_size (t)),
                                    h,
                                    w);
                        }
                        for (int c = 0; c < chans->num_channels; ++c)
                            free (planes[c]);
                        free (counts);
                        continue;
                    }
                    for (int c = 0; c < chans->num_channels; ++c)
                    {
                        const exr_attr_chlist_entry_t* e = chans->entries + c;
                        widths[c]  = tiled ? lw
                                           : sampled (dw.min.x, (int64_t) dw.max.x + 1, e->x_sampling);
                        heights[c] = tiled ? lh
                                           : sampled (dw.min.y, (int64_t) dw.max.y + 1, e->y_sampling);
                        planes[c]  = (uint8_t*) calloc (
                            (size_t) (widths[c] * heights[c] * type_size (t)) + 1, 1);
                        if (!planes[c]) return fail ("out of memory", EXR_ERR_OUT_OF_MEMORY);
                    }
                    rv = levels_decode (f, p, tiled, lx, ly, t, chans, dw, planes, widths);
                    if (rv)
                        fprintf (
                            manifest,
                            "%d\t%d\t%d\t%s\terror\t%s\n",
                            p,
                            lx,
                            ly,
                            type_name (t),
                            exr_get_error_code_as_string (rv));
                    else
                        for (int c = 0; c < chans->num_channels; ++c)
                            levels_write (
                                manifest,
                                argv[1],
                                p,
                                lx,
                                ly,
                                t,
                                c,
                                chans->entries + c,
                                planes[c],
                                (size_t) (heights[c] * widths[c] * type_size (t)),
                                heights[c],
                                widths[c]);
                    for (int c = 0; c < chans->num_channels; ++c)
                        free (planes[c]);
                }
            }
        }
    }
    fclose (manifest);
    exr_finish (&f);
    return 0;
}

int
main (int argc, char** argv)
{
    if (argc >= 2 && !strcmp (argv[1], "levels")) return cmd_levels (argc - 2, argv + 2);
    if (argc >= 2 && !strcmp (argv[1], "compress-buffer"))
        return cmd_compress_buffer (argc - 2, argv + 2);
    if (argc >= 2 && !strcmp (argv[1], "info")) return cmd_info (argc - 2, argv + 2);
    if (argc >= 2 && !strcmp (argv[1], "convert")) return cmd_convert (argc - 2, argv + 2);
    if (argc >= 2 && !strcmp (argv[1], "bench")) return cmd_bench (argc - 2, argv + 2);
    if (argc >= 2 && !strcmp (argv[1], "zstd-big")) return cmd_zstd_big (argc - 2, argv + 2);
    if (argc >= 2 && !strcmp (argv[1], "recompress"))
        return cmd_recompress (argc - 2, argv + 2);
    fprintf (
        stderr,
        "usage: core_harness info <file> [--strict] [--verbose]\n"
        "       core_harness recompress <in> <out> <compression> [--zip N] [--zstd N] [--dwa F]\n"
        "       core_harness compress-buffer <level> <in> <out>\n"
        "       core_harness convert <dir>\n"
        "       core_harness bench <in> <reps> [<compression> [--zip N] [--zstd N] [--dwa F]]\n"
        "       core_harness zstd-big <dir> <width> <level>...\n"
        "       core_harness levels <file> <out-dir>\n");
    return 2;
}
