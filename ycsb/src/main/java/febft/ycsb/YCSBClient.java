package febft.ycsb;

import java.util.*;
import java.io.IOException;
import java.security.Security;

//import org.bouncycastle.jce.provider.BouncyCastleProvider;

import febft.ycsb.Node;
import febft.ycsb.Update;
import febft.ycsb.Config;

import site.ycsb.ByteIterator;
import site.ycsb.Status;
import site.ycsb.DB;

public class YCSBClient extends DB {
    // TODO: change ycsb workload to reflect this new value
    private static final int UPDATE_MAX = Config.getBatchSize();

    private Node node;
    private int updateCount;
    private Update[] updates;

    public YCSBClient() {
        // empty constructor
    }

    // test
    public static void main(String[] args) throws Exception {
        YCSBClient client = new YCSBClient();
        System.err.println("Initializing...");
        client.init();
        System.err.println("Done.");
        Thread.sleep(60 * 60 * 1000);
    }

    @Override
    public void init() {
        //Security.addProvider(new BouncyCastleProvider());

        this.node = new Node();
        int id = this.node.getConfig().getId();

        try {
            node.bootstrap();
        } catch (IOException e) {
            System.err.printf("Failed to bootstrap node %d: %s\n", id, e);
            System.exit(1);
        }

        this.updates = new Update[UPDATE_MAX];
        this.updateCount = 0;
    }

    @Override
    public Status update(String table, String key, Map<String, ByteIterator> values) {
        updates[updateCount++] = new Update(table, key, values);

        if (updateCount % UPDATE_MAX == 0) {
            updateCount = 0;
            try {
                node.println("Calling service");
                return node.callService(updates);
            } catch (IOException e) {
                node.printf("Exception: %s\n", e);
                System.exit(1);
            }
        }

        return Status.OK;
    }

    @Override
    public Status read(String table, String key, Set<String> fields, Map<String, ByteIterator> result) {
        return Status.NOT_IMPLEMENTED;
    }

    @Override
    public Status scan(String table, String startkey, int recordcount, Set<String> fields,
                       Vector<HashMap<String, ByteIterator>> result) {
        return Status.NOT_IMPLEMENTED;
    }

    @Override
    public Status insert(String table, String key, Map<String, ByteIterator> values) {
        return Status.NOT_IMPLEMENTED;
    }

    @Override
    public Status delete(String table, String key) {
        return Status.NOT_IMPLEMENTED;
    }
}
