/* Encodes RPC messages with the C++ app's own nanopb code (plugins/flipperproto0/messages,
 * 3rdparty/nanopb), built the way the app builds it (PB_ENABLE_MALLOC), and prints one
 * "name hex" line per frame. Requests are filled in exactly as the plugin's request classes
 * do (MainRequest: {id, OK, hasNext, {}, tag, {}}); responses the way the firmware fills them.
 * gen.sh builds and runs this; tests/nanopb_fixtures.rs compares the Rust encoding with it. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pb_encode.h>
#include "flipper.pb.h"

static void emit(const char *name, const PB_Main *m)
{
    pb_ostream_t sizing = PB_OSTREAM_SIZING;
    if(!pb_encode_ex(&sizing, &PB_Main_msg, m, PB_ENCODE_DELIMITED)) {
        fprintf(stderr, "%s: sizing failed: %s\n", name, PB_GET_ERROR(&sizing));
        exit(1);
    }
    unsigned char *buf = malloc(sizing.bytes_written);
    pb_ostream_t s = pb_ostream_from_buffer(buf, sizing.bytes_written);
    if(!pb_encode_ex(&s, &PB_Main_msg, m, PB_ENCODE_DELIMITED)) {
        fprintf(stderr, "%s: encode failed: %s\n", name, PB_GET_ERROR(&s));
        exit(1);
    }
    printf("%s ", name);
    for(size_t i = 0; i < s.bytes_written; ++i) {
        printf("%02x", buf[i]);
    }
    printf("\n");
    free(buf);
}

/* MainRequest's constructor, m_message({id, OK, hasNext, {}, tag, {}}): C++ value-initialises
 * the whole union to zero bytes, which C's brace initialiser does not promise; hence memset. */
static PB_Main request(uint32_t id, pb_size_t tag, bool has_next)
{
    PB_Main m;
    memset(&m, 0, sizeof(m));
    m.command_id = id;
    m.command_status = PB_CommandStatus_OK;
    m.has_next = has_next;
    m.which_content = tag;
    return m;
}

static PB_Main response(uint32_t id, pb_size_t tag, bool has_next, PB_CommandStatus status)
{
    PB_Main m;
    memset(&m, 0, sizeof(m));
    m.command_id = id;
    m.command_status = status;
    m.has_next = has_next;
    m.which_content = tag;
    return m;
}

static pb_bytes_array_t *bytes(const char *data, size_t n)
{
    pb_bytes_array_t *b = malloc(PB_BYTES_ARRAY_T_ALLOCSIZE(n));
    b->size = (pb_size_t)n;
    memcpy(b->bytes, data, n);
    return b;
}

int main(void)
{
    /* ---- requests, as ProtobufPlugin encodes them ---- */
    PB_Main m;
    m = request(1, PB_Main_system_protobuf_version_request_tag, false);
    emit("req_protobuf_version", &m);

    char devinfo[] = "devinfo";
    m = request(2, PB_Main_property_get_request_tag, false);
    m.content.property_get_request.key = devinfo;
    emit("req_property_devinfo", &m);

    m = request(3, PB_Main_system_device_info_request_tag, false);
    emit("req_device_info", &m);

    char ext[] = "/ext", root[] = "/", subghz[] = "/ext/subghz", manifest[] = "/ext/Manifest", empty[] = "";
    m = request(4, PB_Main_storage_list_request_tag, false);
    m.content.storage_list_request.path = ext;
    emit("req_list_ext", &m);

    m = request(5, PB_Main_storage_list_request_tag, false);
    m.content.storage_list_request.path = root;
    emit("req_list_root", &m);

    m = request(300, PB_Main_storage_list_request_tag, false);
    m.content.storage_list_request.path = subghz;
    emit("req_list_subghz_id300", &m);

    m = request(6, PB_Main_storage_info_request_tag, false);
    m.content.storage_info_request.path = ext;
    emit("req_info_ext", &m);

    m = request(7, PB_Main_storage_stat_request_tag, false);
    m.content.storage_stat_request.path = manifest;
    emit("req_stat_manifest", &m);

    m = request(8, PB_Main_storage_md5sum_request_tag, false);
    m.content.storage_md5sum_request.path = manifest;
    emit("req_md5_manifest", &m);

    m = request(9, PB_Main_storage_read_request_tag, false);
    m.content.storage_read_request.path = manifest;
    emit("req_read_manifest", &m);

    m = request(10, PB_Main_system_ping_request_tag, false);
    m.content.system_ping_request.data = bytes("abc", 3);
    emit("req_ping_abc", &m);
    free(m.content.system_ping_request.data);

    m = request(11, PB_Main_storage_list_request_tag, false);
    m.content.storage_list_request.path = empty; /* QByteArray().data() is "", never NULL */
    emit("req_list_empty_path", &m);

    m = request(12, PB_Main_system_get_datetime_request_tag, false);
    emit("req_get_datetime", &m);

    m = request(4294967295u, PB_Main_system_protobuf_version_request_tag, false);
    emit("req_protobuf_version_idmax", &m);

    /* ---- requests that change the device (phase 4), as the plugin's request classes ---- */
    char favorites[] = "/int/favorites.txt", sub[] = "/int/sub", renamed[] = "/int/renamed.txt",
         manifest_path[] = "/ext/update/f7-update-test/update.fuf";
    /* StorageWriteRequest: has_file only when the chunk is not empty */
    m = request(20, PB_Main_storage_write_request_tag, true);
    m.content.storage_write_request.path = favorites;
    m.content.storage_write_request.has_file = true;
    m.content.storage_write_request.file.data = bytes("hello", 5);
    emit("req_write_chunk_next", &m);
    free(m.content.storage_write_request.file.data);

    m = request(20, PB_Main_storage_write_request_tag, false);
    m.content.storage_write_request.path = favorites;
    m.content.storage_write_request.has_file = true;
    m.content.storage_write_request.file.data = bytes("!", 1);
    emit("req_write_chunk_last", &m);
    free(m.content.storage_write_request.file.data);

    m = request(21, PB_Main_storage_write_request_tag, false);
    m.content.storage_write_request.path = favorites;
    emit("req_write_empty_file", &m);

    m = request(20, PB_Main_system_ping_request_tag, false);
    emit("req_ping_empty", &m);

    m = request(22, PB_Main_storage_mkdir_request_tag, false);
    m.content.storage_mkdir_request.path = sub;
    emit("req_mkdir_sub", &m);

    m = request(23, PB_Main_storage_delete_request_tag, false);
    m.content.storage_delete_request.path = sub;
    emit("req_delete_sub", &m);

    m = request(24, PB_Main_storage_delete_request_tag, false);
    m.content.storage_delete_request.path = sub;
    m.content.storage_delete_request.recursive = true;
    emit("req_delete_sub_recursive", &m);

    m = request(25, PB_Main_storage_rename_request_tag, false);
    m.content.storage_rename_request.old_path = favorites;
    m.content.storage_rename_request.new_path = renamed;
    emit("req_rename", &m);

    m = request(26, PB_Main_system_reboot_request_tag, false);
    m.content.system_reboot_request.mode = PB_System_RebootRequest_RebootMode_OS;
    emit("req_reboot_os", &m);

    m = request(27, PB_Main_system_reboot_request_tag, false);
    m.content.system_reboot_request.mode = PB_System_RebootRequest_RebootMode_UPDATE;
    emit("req_reboot_update", &m);

    m = request(28, PB_Main_system_factory_reset_request_tag, false);
    emit("req_factory_reset", &m);

    m = request(29, PB_Main_system_update_request_tag, false);
    m.content.system_update_request.update_manifest = manifest_path;
    emit("req_update", &m);

    /* ---- responses, as the firmware fills them ---- */
    m = response(1, PB_Main_system_protobuf_version_response_tag, false, PB_CommandStatus_OK);
    m.content.system_protobuf_version_response.major = 0;
    m.content.system_protobuf_version_response.minor = 25;
    emit("resp_protobuf_version", &m);

    char k[] = "hardware.name", v[] = "Fakeflip";
    m = response(2, PB_Main_property_get_response_tag, true, PB_CommandStatus_OK);
    m.content.property_get_response.key = k;
    m.content.property_get_response.value = v;
    emit("resp_property_pair", &m);

    m = response(2, PB_Main_empty_tag, false, PB_CommandStatus_OK);
    emit("resp_property_end", &m);

    char lk[] = "hardware_name";
    m = response(3, PB_Main_system_device_info_response_tag, false, PB_CommandStatus_OK);
    m.content.system_device_info_response.key = lk;
    m.content.system_device_info_response.value = v;
    emit("resp_device_info_pair", &m);

    char apps[] = "apps", man[] = "Manifest", sig[] = "signal_00.sub";
    m = response(4, PB_Main_storage_list_response_tag, true, PB_CommandStatus_OK);
    PB_Storage_ListResponse *l = &m.content.storage_list_response;
    l->file_count = 3;
    l->file[0].type = PB_Storage_File_FileType_DIR;
    l->file[0].name = apps;
    l->file[1].type = PB_Storage_File_FileType_FILE;
    l->file[1].name = man;
    l->file[1].size = 48213;
    l->file[2].type = PB_Storage_File_FileType_FILE;
    l->file[2].name = sig;
    l->file[2].size = 100;
    emit("resp_list_three", &m);

    m = response(4, PB_Main_storage_list_response_tag, false, PB_CommandStatus_OK);
    emit("resp_list_end_empty", &m);

    m = response(5, PB_Main_empty_tag, false, PB_CommandStatus_ERROR_STORAGE_NOT_EXIST);
    emit("resp_error_not_exist", &m);

    m = response(6, PB_Main_storage_info_response_tag, false, PB_CommandStatus_OK);
    m.content.storage_info_response.total_space = 31902400512ull;
    m.content.storage_info_response.free_space = 30000000000ull;
    emit("resp_info_ext", &m);

    m = response(7, PB_Main_storage_stat_response_tag, false, PB_CommandStatus_OK);
    m.content.storage_stat_response.has_file = true;
    m.content.storage_stat_response.file.type = PB_Storage_File_FileType_FILE;
    m.content.storage_stat_response.file.size = 48213;
    emit("resp_stat_manifest", &m);

    m = response(10, PB_Main_system_ping_response_tag, false, PB_CommandStatus_OK);
    m.content.system_ping_response.data = bytes("abc", 3);
    emit("resp_ping_abc", &m);
    free(m.content.system_ping_response.data);

    /* ---- phase 2: md5, read, date/time ---- */
    char md5hex[] = "d41d8cd98f00b204e9800998ecf8427e";
    m = response(8, PB_Main_storage_md5sum_response_tag, false, PB_CommandStatus_OK);
    memcpy(m.content.storage_md5sum_response.md5sum, md5hex, 33);
    emit("resp_md5", &m);

    m = response(9, PB_Main_storage_read_response_tag, true, PB_CommandStatus_OK);
    m.content.storage_read_response.has_file = true;
    m.content.storage_read_response.file.data = bytes("hello", 5);
    emit("resp_read_chunk", &m);
    free(m.content.storage_read_response.file.data);

    /* the firmware's answer for an empty file: has_file with a zero-length data array */
    m = response(9, PB_Main_storage_read_response_tag, false, PB_CommandStatus_OK);
    m.content.storage_read_response.has_file = true;
    m.content.storage_read_response.file.data = bytes("", 0);
    emit("resp_read_empty_file", &m);
    free(m.content.storage_read_response.file.data);

    m = response(12, PB_Main_system_get_datetime_response_tag, false, PB_CommandStatus_OK);
    m.content.system_get_datetime_response.has_datetime = true;
    m.content.system_get_datetime_response.datetime.year = 2026;
    m.content.system_get_datetime_response.datetime.month = 11;
    m.content.system_get_datetime_response.datetime.day = 9;
    m.content.system_get_datetime_response.datetime.hour = 12;
    m.content.system_get_datetime_response.datetime.minute = 34;
    m.content.system_get_datetime_response.datetime.second = 56;
    m.content.system_get_datetime_response.datetime.weekday = 1;
    emit("resp_datetime", &m);

    /* StorageReadOperation: stat and read under one id, written back to back */
    char favs[] = "/int/favorites.txt";
    m = request(13, PB_Main_storage_stat_request_tag, false);
    m.content.storage_stat_request.path = favs;
    emit("req_read_pair_stat", &m);
    m = request(13, PB_Main_storage_read_request_tag, false);
    m.content.storage_read_request.path = favs;
    emit("req_read_pair_read", &m);

    return 0;
}
